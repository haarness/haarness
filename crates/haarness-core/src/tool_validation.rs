//! Shared model-call validation and conservative name recovery.

use jsonschema::error::{TypeKind, ValidationErrorKind};
use serde_json::Value;

use crate::{CoreError, CoreResult, ToolDefinition};

pub(crate) fn resolve_name(name: &str, names: impl IntoIterator<Item = String>) -> String {
    let mut names: Vec<_> = names.into_iter().collect();
    names.sort();
    names.dedup();
    let local = name
        .get(..10)
        .filter(|prefix| prefix.eq_ignore_ascii_case("functions."))
        .map(|_| &name[10..]);
    for candidate in std::iter::once(name).chain(local) {
        if names.iter().any(|registered| registered == candidate) {
            return candidate.to_owned();
        }
        let mut matches = names
            .iter()
            .filter(|registered| registered.eq_ignore_ascii_case(candidate));
        if let Some(found) = matches.next() {
            return if matches.next().is_none() {
                found.clone()
            } else {
                name.to_owned()
            };
        }
    }
    name.to_owned()
}

pub(crate) fn unknown_tool(name: &str, names: impl IntoIterator<Item = String>) -> CoreError {
    let mut names: Vec<_> = names.into_iter().collect();
    names.sort();
    names.dedup();
    // Bound feedback, never the executable catalog.
    let more = if names.len() > 12 {
        " (showing 12)"
    } else {
        ""
    };
    let available = if names.is_empty() {
        "No tools are currently available.".to_owned()
    } else {
        format!(
            "Available tools{more}: {}.",
            names
                .iter()
                .take(12)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    CoreError::Tool(format!(
        "unknown tool: {name}. Use an exact available tool name. {available}"
    ))
}

pub(crate) fn validate_arguments(tool: &ToolDefinition, arguments: &Value) -> CoreResult<()> {
    // No HTTP/file resolver features are enabled. A broken tool schema is a
    // configuration error, not permission to execute an unvalidated payload.
    let validator = jsonschema::validator_for(&tool.input_schema).map_err(|error| {
        CoreError::Tool(format!(
            "tool {} has an invalid input schema at {} ({}). The tool definition must be fixed before retrying.",
            tool.name, error.schema_path(), error.kind().keyword()
        ))
    })?;
    let mut errors = validator.iter_errors(arguments);
    let mut issues = Vec::new();
    for error in errors.by_ref().take(8) {
        let mut path = argument_path(error.instance_path().as_str(), arguments);
        let message = match error.kind() {
            ValidationErrorKind::Required { property } => {
                if let Some(property) = property.as_str() {
                    append_property(&mut path, property);
                }
                "required field is missing".into()
            }
            ValidationErrorKind::Type { kind } => match kind {
                TypeKind::Single(kind) => format!("expected {kind}"),
                TypeKind::Multiple(kinds) => format!("expected one of {kinds:?}"),
            },
            ValidationErrorKind::Enum { options } => format!("expected one of {options}"),
            ValidationErrorKind::Constant { expected_value } => {
                format!("expected {expected_value}")
            }
            ValidationErrorKind::AdditionalProperties { unexpected }
            | ValidationErrorKind::UnevaluatedProperties { unexpected } => {
                format!("unexpected fields: {}", unexpected.join(", "))
            }
            ValidationErrorKind::MinLength { limit } => {
                format!("expected at least {limit} characters")
            }
            ValidationErrorKind::MaxLength { limit } => {
                format!("expected at most {limit} characters")
            }
            ValidationErrorKind::MinItems { limit } => format!("expected at least {limit} items"),
            ValidationErrorKind::MaxItems { limit } => format!("expected at most {limit} items"),
            ValidationErrorKind::Minimum { limit } => format!("expected a value >= {limit}"),
            ValidationErrorKind::Maximum { limit } => format!("expected a value <= {limit}"),
            ValidationErrorKind::Pattern { pattern } => format!("must match pattern {pattern}"),
            ValidationErrorKind::AnyOf { .. } => {
                "must match at least one allowed argument shape".into()
            }
            ValidationErrorKind::OneOfNotValid { .. }
            | ValidationErrorKind::OneOfMultipleValid { .. } => {
                "must match exactly one allowed argument shape".into()
            }
            kind => format!("does not satisfy {}", kind.keyword()),
        };
        let path = if path.is_empty() { "<root>" } else { &path };
        // Do not echo rejected values (file contents, credentials, etc.).
        issues.push(format!("- {path}: {message}"));
    }
    if issues.is_empty() {
        return Ok(());
    }
    if errors.next().is_some() {
        issues.push("- Additional schema issues omitted; fix these fields first.".into());
    }
    Err(CoreError::Invalid(format!(
        "The {} tool was called with invalid arguments. Fix the input and retry the same tool.\n{}",
        tool.name,
        issues.join("\n")
    )))
}

fn argument_path(pointer: &str, arguments: &Value) -> String {
    let mut path = String::new();
    let mut value = Some(arguments);
    for part in pointer.split('/').skip(1) {
        let part = part.replace("~1", "/").replace("~0", "~");
        if value.is_some_and(Value::is_array) {
            use std::fmt::Write as _;
            let _ = write!(path, "[{part}]");
            value = part
                .parse::<usize>()
                .ok()
                .and_then(|index| value?.get(index));
        } else {
            append_property(&mut path, &part);
            value = value.and_then(|value| value.get(&part));
        }
    }
    path
}

fn append_property(path: &mut String, property: &str) {
    use std::fmt::Write as _;
    if !property.is_empty()
        && property.chars().enumerate().all(|(index, ch)| {
            ch == '_' || ch.is_ascii_alphabetic() || (index > 0 && ch.is_ascii_digit())
        })
    {
        if !path.is_empty() {
            path.push('.');
        }
        path.push_str(property);
    } else {
        let _ = write!(
            path,
            "[{}]",
            serde_json::to_string(property).expect("string is serializable")
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn definition(schema: Value) -> ToolDefinition {
        ToolDefinition {
            name: "fixture".into(),
            description: String::new(),
            input_schema: schema,
        }
    }

    #[test]
    fn names_preserve_exact_matches_and_reject_ambiguity_and_guesses() {
        let resolve = |name| {
            resolve_name(
                name,
                ["read", "Read", "glob", "functions.Glob", "load_tool"].map(str::to_owned),
            )
        };
        assert_eq!(resolve("Read"), "Read");
        assert_eq!(resolve("READ"), "READ");
        assert_eq!(resolve("functions.READ"), "functions.READ");
        assert_eq!(resolve("GLOB"), "glob");
        assert_eq!(resolve("functions.glob"), "functions.Glob");
        assert_eq!(resolve("FUNCTIONS.LOAD_TOOL"), "load_tool");
        assert_eq!(resolve("glbo"), "glbo");
        assert_eq!(resolve("other.glob"), "other.glob");
        assert_eq!(resolve(" glob "), " glob ");
    }

    #[test]
    fn validation_reports_nested_paths_without_echoing_invalid_values() {
        let tool = definition(
            json!({"type":"object","properties":{"calls":{"type":"array","items":{
            "type":"object","properties":{"pattern":{"type":"string"},"count":{"type":"integer"}},
            "required":["pattern"],"additionalProperties":false
        }}},"required":["calls"]}),
        );
        let error = validate_arguments(
            &tool,
            &json!({"calls":[{}, {"pattern":42,"count":"secret-value"}]}),
        )
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("calls[0].pattern: required field is missing"),
            "{error}"
        );
        assert!(
            error.contains("calls[1].pattern: expected string"),
            "{error}"
        );
        assert!(
            error.contains("calls[1].count: expected integer"),
            "{error}"
        );
        assert!(!error.contains("secret-value"));
    }

    #[test]
    fn validation_handles_root_escaped_properties_refs_and_constraints() {
        let tool = definition(
            json!({"$defs":{"value":{"type":"string","enum":["yes","no"]}},
            "type":"object","properties":{"a/b":{"$ref":"#/$defs/value"},"0":{"type":"integer","minimum":1}},
            "required":["a/b"],"additionalProperties":false}),
        );
        assert!(validate_arguments(&tool, &json!({"a/b":"yes","0":1})).is_ok());
        let error = validate_arguments(&tool, &json!({"a/b":"maybe","0":0}))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("[\"a/b\"]: expected one of [\"yes\",\"no\"]"),
            "{error}"
        );
        assert!(error.contains("[\"0\"]: expected a value >= 1"), "{error}");
        assert!(
            validate_arguments(&tool, &Value::Null)
                .unwrap_err()
                .to_string()
                .contains("<root>: expected object")
        );
    }

    #[test]
    fn valid_optional_and_additional_arguments_are_not_rewritten() {
        let tool = definition(
            json!({"type":"object","properties":{"limit":{"type":["integer","null"]}},"additionalProperties":true}),
        );
        for arguments in [json!({}), json!({"limit":null,"extra":[1,2,3]})] {
            let original = arguments.clone();
            validate_arguments(&tool, &arguments).unwrap();
            assert_eq!(arguments, original);
        }
    }

    #[test]
    fn invalid_tool_schema_fails_without_guessing_arguments() {
        let error = validate_arguments(&definition(json!({"type":"not-a-json-type"})), &json!({}))
            .unwrap_err()
            .to_string();
        assert!(error.contains("invalid input schema"), "{error}");
        assert!(error.contains("tool definition must be fixed"), "{error}");
    }

    #[test]
    fn validation_feedback_is_bounded_without_limiting_arguments() {
        let tool = definition(json!({"type":"array","items":{"type":"integer"}}));
        let error = validate_arguments(&tool, &json!(vec!["wrong"; 20]))
            .unwrap_err()
            .to_string();
        assert_eq!(
            error
                .lines()
                .filter(|line| line.contains(": expected integer"))
                .count(),
            8
        );
        assert!(error.contains("Additional schema issues omitted"));
        validate_arguments(&tool, &json!(vec![1; 10000])).unwrap();
    }
}

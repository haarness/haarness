//! Tool schemas Claude refuses. Claude validates every tool before inference
//! and answers HTTP 400 for the whole request when one root input schema
//! combines alternatives, so a single such MCP tool would fail every turn.
//! Adapters that reach Claude omit those tools and warn once per tool name.

use std::{
    collections::{HashMap, HashSet},
    sync::{LazyLock, Mutex},
};

use haarness_core::ToolDefinition;
use serde_json::{Map, Value};

/// Root keywords Claude rejects in a tool's `input_schema`. Nested uses and
/// properties with these names are accepted.
const ROOT_COMBINERS: [&str; 3] = ["oneOf", "anyOf", "allOf"];

static WARNED: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(Mutex::default);

/// Claude models that answer 400 to a forced tool choice (`any` or a named
/// tool), under any vendor prefix: `claude-opus-5-5`, `anthropic/claude-sonnet-5.5`,
/// `us.anthropic.claude-fable-5-1-v1:0`. Structured output then relies on the
/// runtime's request to call the tool and its corrective retry, so a release
/// newer than any Haarness knows is treated as refusing: unforced, it still
/// answers in the schema's shape. Fable and Mythos refuse from 5.1, the
/// other families from 5.5; Haiku 5.5 still takes a forced choice.
pub(crate) fn refuses_forced_tool_choice(model: &str) -> bool {
    let Some(release) = crate::anthropic::claude_release(model) else {
        return false;
    };
    match release.family.as_str() {
        "fable" | "mythos" => release.version >= (5, 1),
        "haiku" => release.version > crate::anthropic::NEWEST_CLAUDE,
        _ => release.version >= (5, 5),
    }
}

pub(crate) fn claude_model(model: &str) -> bool {
    model.to_ascii_lowercase().contains("claude")
}

/// Why Claude would reject `schema` as a tool's root input schema.
pub(crate) fn claude_schema_issues(schema: &Value) -> Vec<String> {
    let Some(root) = schema.as_object() else {
        return vec!["the input schema is not a JSON object".into()];
    };
    let mut issues = ROOT_COMBINERS
        .iter()
        .filter(|key| root.contains_key(**key))
        .map(|key| format!("{key}: unsupported at the top level by Claude tool schema"))
        .collect::<Vec<_>>();
    // A missing type is left alone; MCP definitions fill it in.
    if root.get("type").is_some_and(|kind| kind != "object") {
        issues.push("type: Claude tool input must be an object".into());
    }
    issues
}

/// Drop the tools Claude would reject when `model` is a Claude model.
pub(crate) fn retain_claude_compatible(model: &str, tools: &mut Vec<ToolDefinition>) {
    if !claude_model(model) {
        return;
    }
    tools.retain(|tool| {
        let issues = claude_schema_issues(&tool.input_schema);
        if issues.is_empty() {
            return true;
        }
        if first_warning(&tool.name) {
            tracing::warn!(
                tool = %tool.name,
                issues = %issues.join("; "),
                "skipping provider-incompatible tool schema for Claude"
            );
        }
        false
    });
}

/// Keywords whose values are data rather than schemas: never rewritten.
const SCHEMA_DATA: [&str; 5] = ["const", "default", "enum", "examples", "dependentRequired"];

/// Keywords whose values map names to schemas.
const SCHEMA_MAPS: [&str; 6] = [
    "properties",
    "patternProperties",
    "$defs",
    "definitions",
    "dependentSchemas",
    "dependencies",
];

/// `schema` in the shape Gemini's function declarations take. Gemini reads
/// standard JSON Schema, but answers the whole request with a 400 over a few
/// shapes that MCP servers' tools often carry, and a chat that had loaded
/// such a tool failed every turn after on every Gemini model:
///
/// - `required` naming a property the schema does not declare ("schema at
///   top-level requires unspecified property"), which is declared as taking
///   any value;
/// - draft-04 boolean `exclusiveMinimum`/`exclusiveMaximum` ("must be a
///   number"), which become the numeric bound;
/// - draft-07 tuples, an `items` list with `additionalItems` ("items must be
///   a boolean or an object"), which become `prefixItems` and `items`;
/// - a `$ref` loop through required places only ("ref loops are only
///   supported if they include optional or nullable property values, or a
///   potentially-zero-length array items"): the reference closing the loop
///   accepts any value.
///
/// Everything else is sent as it is. The runtime still checks each call
/// against the tool's own schema.
pub(crate) fn gemini_compatible(schema: Value) -> Value {
    let mut schema = gemini_node(schema);
    cut_required_loops(&mut schema);
    schema
}

/// Whether a request for `model` (under any gateway prefix) reaches Gemini,
/// which refuses the shapes [`gemini_compatible`] rewrites.
pub(crate) fn gemini_model(model: &str) -> bool {
    model
        .rsplit('/')
        .next()
        .is_some_and(|name| name.to_ascii_lowercase().starts_with("gemini-"))
}

fn gemini_node(value: Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.into_iter().map(gemini_node).collect()),
        Value::Object(object) => Value::Object(gemini_object(object)),
        other => other,
    }
}

fn gemini_object(mut object: Map<String, Value>) -> Map<String, Value> {
    for (exclusive, bound) in [
        ("exclusiveMinimum", "minimum"),
        ("exclusiveMaximum", "maximum"),
    ] {
        if let Some(Value::Bool(exclusive_bound)) = object.get(exclusive) {
            let exclusive_bound = *exclusive_bound;
            object.remove(exclusive);
            if exclusive_bound && let Some(limit) = object.remove(bound) {
                if limit.is_number() {
                    object.insert(exclusive.into(), limit);
                } else {
                    object.insert(bound.into(), limit);
                }
            }
        }
    }
    if object.get("items").is_some_and(Value::is_array) {
        let items = object.remove("items").unwrap_or_default();
        object.entry("prefixItems").or_insert(items);
        if let Some(rest) = object.remove("additionalItems") {
            object.insert("items".into(), rest);
        }
    }
    let object_type = match object.get("type") {
        Some(Value::String(kind)) => kind == "object",
        Some(Value::Array(kinds)) => kinds.iter().any(|kind| kind == "object"),
        _ => false,
    };
    // A required name the object does not declare is declared, taking any
    // value: the tool still needs it, and the call is checked against the
    // tool's own schema.
    let declares = object_type || object.get("properties").is_some_and(Value::is_object);
    if declares && let Some(Value::Array(required)) = object.get("required") {
        let mut names: Vec<Value> = Vec::new();
        for name in required.iter().filter(|name| name.is_string()) {
            if !names.contains(name) {
                names.push(name.clone());
            }
        }
        if let Value::Object(properties) = object
            .entry("properties")
            .or_insert_with(|| Value::Object(Map::new()))
        {
            for name in names.iter().filter_map(Value::as_str) {
                properties
                    .entry(name)
                    .or_insert_with(|| Value::Object(Map::new()));
            }
        }
        if names.is_empty() {
            object.remove("required");
        } else {
            object.insert("required".into(), Value::Array(names));
        }
    }
    for (key, value) in &mut object {
        if SCHEMA_DATA.contains(&key.as_str()) {
            continue;
        }
        let taken = std::mem::take(value);
        *value = match taken {
            Value::Object(map) if SCHEMA_MAPS.contains(&key.as_str()) => Value::Object(
                map.into_iter()
                    .map(|(name, schema)| (name, gemini_node(schema)))
                    .collect(),
            ),
            other => gemini_node(other),
        };
    }
    object
}

/// The names a `$ref` gives the root (`#`) and each of its definitions.
fn definition_refs(root: &Value) -> Vec<(String, String)> {
    let escape = |name: &str| name.replace('~', "~0").replace('/', "~1");
    let mut refs = vec![("#".to_owned(), String::new())];
    for section in ["$defs", "definitions"] {
        if let Some(Value::Object(definitions)) = root.get(section) {
            for name in definitions.keys() {
                let pointer = format!("/{section}/{}", escape(name));
                refs.push((format!("#{pointer}"), pointer));
            }
        }
    }
    refs
}

/// How far the walk for reference loops has come with a definition.
#[derive(Clone, Copy, PartialEq)]
enum Mark {
    New,
    Walking,
    Done,
}

/// Walk the definitions `at` reaches; a reference to one still being walked
/// closes a loop, and where it is goes into `cut`.
fn walk(at: usize, edges: &[Vec<(String, usize)>], marks: &mut [Mark], cut: &mut Vec<String>) {
    marks[at] = Mark::Walking;
    for (pointer, target) in &edges[at] {
        match marks[*target] {
            Mark::Walking => cut.push(pointer.clone()),
            Mark::New => walk(*target, edges, marks, cut),
            Mark::Done => {}
        }
    }
    marks[at] = Mark::Done;
}

/// Replace with `{}` each `$ref` that closes a loop of references through
/// required places only, the loops Gemini refuses.
fn cut_required_loops(root: &mut Value) {
    let refs = definition_refs(root);
    let index: HashMap<&str, usize> = refs
        .iter()
        .enumerate()
        .map(|(at, (reference, _))| (reference.as_str(), at))
        .collect();
    // For each definition, the references it reaches through required
    // places: where the `$ref` is, and which definition it names.
    let edges: Vec<Vec<(String, usize)>> = refs
        .iter()
        .map(|(_, pointer)| {
            let mut found = Vec::new();
            if let Some(node) = root.pointer(pointer) {
                required_refs(node, pointer, pointer.is_empty(), &mut found);
            }
            found
                .into_iter()
                .filter_map(|(at, target)| Some((at, *index.get(target.as_str())?)))
                .collect()
        })
        .collect();
    let mut marks = vec![Mark::New; refs.len()];
    let mut cut = Vec::new();
    for at in 0..refs.len() {
        if marks[at] == Mark::New {
            walk(at, &edges, &mut marks, &mut cut);
        }
    }
    for pointer in cut {
        if let Some(node) = root.pointer_mut(&pointer) {
            *node = Value::Object(Map::new());
        }
    }
}

/// The `$ref`s under `node` (at JSON pointer `pointer`) that a value must
/// reach: not through an optional property, a map of optional names, or the
/// items of an array that may be empty. The root's definitions are walked on
/// their own.
fn required_refs(node: &Value, pointer: &str, root: bool, found: &mut Vec<(String, String)>) {
    let escape = |name: &str| name.replace('~', "~0").replace('/', "~1");
    match node {
        Value::Array(items) => {
            for (at, item) in items.iter().enumerate() {
                required_refs(item, &format!("{pointer}/{at}"), false, found);
            }
        }
        Value::Object(object) => {
            if let Some(Value::String(target)) = object.get("$ref") {
                found.push((pointer.to_owned(), target.clone()));
            }
            let required: HashSet<&str> = object
                .get("required")
                .and_then(Value::as_array)
                .map(|names| names.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            let least_items = object.get("minItems").and_then(Value::as_u64).unwrap_or(0);
            for (key, value) in object {
                let child = format!("{pointer}/{}", escape(key));
                match key.as_str() {
                    "$ref" => {}
                    "$defs" | "definitions" if root => {}
                    "properties" => {
                        if let Value::Object(properties) = value {
                            for (name, schema) in properties {
                                if required.contains(name.as_str()) {
                                    let at = format!("{child}/{}", escape(name));
                                    required_refs(schema, &at, false, found);
                                }
                            }
                        }
                    }
                    "items" | "contains" if least_items > 0 => {
                        required_refs(value, &child, false, found);
                    }
                    "prefixItems" => {
                        if let Value::Array(items) = value {
                            let least = usize::try_from(least_items).unwrap_or(usize::MAX);
                            for (at, item) in items.iter().take(least).enumerate() {
                                required_refs(item, &format!("{child}/{at}"), false, found);
                            }
                        }
                    }
                    key if SCHEMA_DATA.contains(&key) => {}
                    "items"
                    | "contains"
                    | "patternProperties"
                    | "additionalProperties"
                    | "propertyNames"
                    | "dependentSchemas"
                    | "dependencies"
                    | "not"
                    | "$defs"
                    | "definitions" => {}
                    _ => required_refs(value, &child, false, found),
                }
            }
        }
        _ => {}
    }
}

/// The same tool is offered on every request; one warning per process names it.
fn first_warning(tool: &str) -> bool {
    WARNED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(tool.to_owned())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn only_root_combiners_and_non_object_roots_are_rejected() {
        for key in ROOT_COMBINERS {
            let schema = json!({"type":"object", key: [{"required":["a"]},{"required":["b"]}]});
            assert_eq!(
                claude_schema_issues(&schema),
                [format!(
                    "{key}: unsupported at the top level by Claude tool schema"
                )]
            );
            let nested = json!({"type":"object","properties":{
                "value": {key: [{"type":"string"},{"type":"integer"}]},
                key: {"type":"string"}
            }});
            assert!(claude_schema_issues(&nested).is_empty());
        }
        assert!(claude_schema_issues(&json!({"properties":{}})).is_empty());
        assert_eq!(claude_schema_issues(&json!({"type":"string"})).len(), 1);
        assert_eq!(claude_schema_issues(&json!(true)).len(), 1);
    }

    #[test]
    fn claude_models_are_recognized_across_vendors() {
        for model in [
            "claude-opus-5-5",
            "anthropic/claude-sonnet-5.5",
            "us.anthropic.claude-sonnet-4-6-v1:0",
            "Claude-Haiku",
        ] {
            assert!(claude_model(model), "{model}");
        }
        for model in ["MiniMax-M3", "gpt-6.1-sol", "amazon.nova-pro-v1:0"] {
            assert!(!claude_model(model), "{model}");
        }
        for model in [
            "claude-opus-5-5",
            "anthropic/claude-sonnet-5.5",
            "us.anthropic.claude-fable-5-1-v1:0",
            "claude-mythos-5-1",
            // Vertex AI's dated ids and a context mark name the same release.
            "claude-opus-5-5@20260901",
            "claude-sonnet-5-5[1m]",
            "claude-fable-5-1@20260801",
        ] {
            assert!(refuses_forced_tool_choice(model), "{model}");
        }
        for model in [
            "claude-sonnet-5",
            "claude-sonnet-5@20260301",
            "claude-opus-4-7[1m]",
            "claude-haiku-5-5@20260901",
            "anthropic/claude-haiku-5.5",
            "gpt-6.1-sol",
            "claude-3-5-sonnet-20241022",
            "claude-opus-4-20250514",
        ] {
            assert!(!refuses_forced_tool_choice(model), "{model}");
        }
        // A release newer than any listed takes the newest behaviour.
        for model in [
            "claude-opus-6",
            "anthropic/claude-sonnet-5.7",
            "claude-haiku-6-20270101",
            "global.anthropic.claude-nova-6-1-v1:0",
        ] {
            assert!(refuses_forced_tool_choice(model), "{model}");
        }
    }

    /// The shapes Gemini answered with a 400 (live, 2026-11-10), each
    /// rewritten the way it accepts.
    #[test]
    fn gemini_schemas_lose_only_the_shapes_gemini_refuses() {
        // A required property the schema does not declare is declared, taking
        // any value, at any depth.
        assert_eq!(
            gemini_compatible(json!({"type":"object","properties":{
                "word":{"type":"string"},
                "inner":{"type":"object","properties":{"a":{"type":"string"}},"required":["a","b"]},
                "bare":{"type":"object","required":["x"]}
            },"required":["word","language"]})),
            json!({"type":"object","properties":{
                "word":{"type":"string"},
                "inner":{"type":"object","properties":{"a":{"type":"string"},"b":{}},"required":["a","b"]},
                "bare":{"type":"object","properties":{"x":{}},"required":["x"]},
                "language":{}
            },"required":["word","language"]})
        );
        // Draft-04 boolean bounds become the numeric form.
        assert_eq!(
            gemini_compatible(json!({"type":"object","properties":{"value":{
                "type":"number","minimum":0,"exclusiveMinimum":true,"maximum":100,"exclusiveMaximum":false
            }}})),
            json!({"type":"object","properties":{"value":{
                "type":"number","exclusiveMinimum":0,"maximum":100
            }}})
        );
        // A draft-07 tuple is `prefixItems`, and what may follow it `items`.
        assert_eq!(
            gemini_compatible(json!({"type":"object","properties":{"pair":{
                "type":"array","items":[{"type":"string"},{"type":"integer","exclusiveMinimum":true,"minimum":1}],"additionalItems":false
            }}})),
            json!({"type":"object","properties":{"pair":{
                "type":"array","prefixItems":[{"type":"string"},{"type":"integer","exclusiveMinimum":1}],"items":false
            }}})
        );
        // Values that are data keep every key, `required` and `items` too;
        // a property may be named like a keyword.
        let data = json!({"type":"object","properties":{
            "items":{"type":"array","items":{"type":"string"}},
            "mode":{"enum":[{"required":["x"]}],"default":{"items":[1,2]},"examples":[{"exclusiveMinimum":true}]}
        },"required":["items"]});
        assert_eq!(gemini_compatible(data.clone()), data);
        // What Gemini takes is sent as it is.
        let accepted = json!({"$schema":"http://json-schema.org/draft-07/schema#","type":"object","properties":{
            "a":{"type":["string","null"],"format":"uri"},
            "b":{"anyOf":[{"type":"string"},{"type":"integer"}]},
            "c":{"type":"object","additionalProperties":{"type":"string"}},
            "next":{"$ref":"#"}
        },"required":["a"]});
        assert_eq!(gemini_compatible(accepted.clone()), accepted);
    }

    #[test]
    fn gemini_schemas_lose_only_reference_loops_through_required_places() {
        // A node that must hold another of itself: the reference closing
        // the loop takes any value. One through an optional property, or
        // through an array that may be empty, stays.
        assert_eq!(
            gemini_compatible(
                json!({"type":"object","properties":{"root":{"$ref":"#/$defs/Node"}},"required":["root"],
                "$defs":{
                    "Node":{"type":"object","properties":{"title":{"type":"string"},"child":{"$ref":"#/$defs/Node"},"more":{"$ref":"#/$defs/Node"}},"required":["title","child"]},
                    "Tree":{"type":"object","properties":{"kids":{"type":"array","items":{"$ref":"#/$defs/Tree"}}},"required":["kids"]},
                    "Full":{"type":"object","properties":{"kids":{"type":"array","minItems":1,"items":{"$ref":"#/$defs/Full"}}},"required":["kids"]}
                }})
            ),
            json!({"type":"object","properties":{"root":{"$ref":"#/$defs/Node"}},"required":["root"],
            "$defs":{
                "Node":{"type":"object","properties":{"title":{"type":"string"},"child":{},"more":{"$ref":"#/$defs/Node"}},"required":["title","child"]},
                "Tree":{"type":"object","properties":{"kids":{"type":"array","items":{"$ref":"#/$defs/Tree"}}},"required":["kids"]},
                "Full":{"type":"object","properties":{"kids":{"type":"array","minItems":1,"items":{}}},"required":["kids"]}
            }})
        );
        // Two definitions that require each other, a choice that includes
        // null on the way (Gemini still refuses it), and the root itself.
        let mutual = gemini_compatible(
            json!({"type":"object","properties":{"a":{"$ref":"#/definitions/A"}},
            "definitions":{
                "A":{"type":"object","properties":{"b":{"$ref":"#/definitions/B"}},"required":["b"]},
                "B":{"type":"object","properties":{"a":{"anyOf":[{"$ref":"#/definitions/A"},{"type":"null"}]}},"required":["a"]}
            }}),
        );
        assert_eq!(
            mutual["definitions"]["A"]["properties"]["b"],
            json!({"$ref":"#/definitions/B"})
        );
        assert_eq!(
            mutual["definitions"]["B"]["properties"]["a"]["anyOf"][0],
            json!({})
        );
        assert_eq!(
            gemini_compatible(
                json!({"type":"object","properties":{"child":{"$ref":"#"}},"required":["child"]})
            ),
            json!({"type":"object","properties":{"child":{}},"required":["child"]})
        );
    }

    #[test]
    fn gemini_is_recognized_through_gateways() {
        for model in [
            "gemini-3.8-flash",
            "google/gemini-4-argon",
            "publishers/google/models/Gemini-3.1-pro",
        ] {
            assert!(gemini_model(model), "{model}");
        }
        for model in [
            "gemma-3-27b-it",
            "anthropic/claude-haiku-5.5",
            "google/palm-2",
        ] {
            assert!(!gemini_model(model), "{model}");
        }
    }

    #[test]
    fn each_skipped_tool_warns_once_per_process() {
        // No other test offers this tool, so the process has not warned yet.
        let name = "mcp__warn_once_fixture__tool".to_owned();
        assert!(first_warning(&name));
        assert!(!first_warning(&name));
        let mut tools = vec![ToolDefinition {
            name: name.clone(),
            description: String::new(),
            input_schema: json!({"anyOf": []}),
        }];
        retain_claude_compatible("claude-test", &mut tools);
        assert!(tools.is_empty());
    }
}

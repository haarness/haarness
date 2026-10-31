//! A corpus of shell lines read and decided the way a `bash` call is, under
//! three configurations with and without remembered answers, so one build's
//! reading of them can be compared with another's
//! (`scripts/permission-corpus.py` runs it and compares two runs).
//!
//! It runs on demand only: `HAARNESS_PERMISSION_CORPUS` names a JSON list of
//! command lines, `HAARNESS_PERMISSION_CORPUS_PROJECT` the project folder
//! they run in and `HAARNESS_PERMISSION_CORPUS_OUT` the file that gets one
//! JSON object per line.
use super::*;
use crate::permissions::bash;

fn rule(pattern: &str, decision: PermissionDecision) -> PermissionRule {
    PermissionRule {
        pattern: pattern.into(),
        decision,
    }
}

/// The configurations every line is decided under: Haarness's defaults, a
/// person who allows a short list of commands and asks about the rest, and
/// one who denies `rm`.
fn configurations() -> Vec<(&'static str, Vec<PermissionRule>)> {
    use PermissionDecision::{Allow, Ask, Deny};
    let mut allowlist = vec![rule("*", Ask)];
    for command in [
        "git status *",
        "git diff *",
        "git log *",
        "ls *",
        "cat *",
        "echo *",
        "grep *",
        "npm run *",
        "npm test *",
        "cargo build *",
        "cargo test *",
        "cd *",
        "true *",
        "mkdir *",
    ] {
        allowlist.push(rule(&format!("bash:{command}"), Allow));
    }
    vec![
        ("default", Vec::new()),
        ("allowlist", allowlist),
        ("rm-deny", vec![rule("bash:rm *", Deny)]),
    ]
}

/// Answers a person might have given Always to earlier.
fn remembered() -> Vec<PermissionRule> {
    let tmp = dunce::canonicalize(Path::new("/tmp")).unwrap_or_else(|_| PathBuf::from("/tmp"));
    let mut grants: Vec<PermissionRule> = [
        "bash:git status *",
        "bash:npm run *",
        "bash:cat *",
        "bash:cargo *",
        "bash:ls *",
    ]
    .into_iter()
    .map(|pattern| rule(pattern, PermissionDecision::Allow))
    .collect();
    grants.push(rule(
        &format!("external_directory:{}/*", tmp.display()),
        PermissionDecision::Allow,
    ));
    grants
}

/// What a `bash` call of `command` would do before running: refused by a
/// rule, asked about (with the patterns and the Always it would offer), or
/// allowed.
fn decide(engine: &PermissionEngine, command: &str, workspace: &Path) -> serde_json::Value {
    let mut asked = Vec::new();
    let mut always = Vec::new();
    for plan in ask_plans("bash", &json!({ "command": command }), workspace) {
        let mut asks = false;
        for pattern in &plan.patterns {
            let evaluation = plan.evaluate(engine, pattern);
            match evaluation.decision {
                PermissionDecision::Deny => {
                    return json!({
                        "decision": "deny",
                        "pattern": plan.operation(pattern),
                        "rule": evaluation.matched,
                    });
                }
                PermissionDecision::Ask => {
                    asks = true;
                    asked.push(plan.operation(pattern));
                }
                PermissionDecision::Allow => {}
            }
        }
        if asks {
            always.extend(plan.always_rules());
        }
    }
    json!({
        "decision": if asked.is_empty() { "allow" } else { "ask" },
        "asked": asked,
        "always": always,
    })
}

/// Everything the shell reading says about `command`.
fn reading(command: &str, workspace: &Path) -> serde_json::Value {
    let analysis = bash::analyze(command, workspace);
    json!({
        "patterns": analysis.patterns,
        "always": analysis.always,
        "external_dirs": analysis.external_dirs,
        "written": analysis
            .written
            .iter()
            .map(|written| json!([written.pattern, written.always, written.incomplete]))
            .collect::<Vec<_>>(),
        "derived": analysis
            .derived
            .iter()
            .map(|derived| json!([derived.pattern, derived.written, derived.always]))
            .collect::<Vec<_>>(),
        "unreadable": analysis.unreadable,
        "deletions": analysis
            .deletions
            .iter()
            .map(|deletion| {
                json!([
                    deletion.raw,
                    deletion.value,
                    deletion.complete,
                    format!("{:?}", deletion.effect),
                    deletion.at,
                    deletion.moved,
                ])
            })
            .collect::<Vec<_>>(),
        "changes_directory": analysis.changes_directory,
        "directories": analysis.directories,
        "links": analysis.links,
        "outside_deletions": bash::outside_deletions(command, workspace, None)
            .iter()
            .map(|deletion| json!([deletion.path, deletion.placed, format!("{:?}", deletion.effect)]))
            .collect::<Vec<_>>(),
        "read_only": bash::is_obviously_read_only(command),
        "scripts": bash::script_files(command, workspace)
            .iter()
            .map(|script| json!([script.path, format!("{:?}", script.withheld)]))
            .collect::<Vec<_>>(),
    })
}

#[test]
#[ignore = "a corpus run on demand: see the module comment"]
fn permission_corpus() {
    let (Ok(corpus), Ok(project), Ok(out)) = (
        std::env::var("HAARNESS_PERMISSION_CORPUS"),
        std::env::var("HAARNESS_PERMISSION_CORPUS_PROJECT"),
        std::env::var("HAARNESS_PERMISSION_CORPUS_OUT"),
    ) else {
        panic!(
            "set HAARNESS_PERMISSION_CORPUS, HAARNESS_PERMISSION_CORPUS_PROJECT and HAARNESS_PERMISSION_CORPUS_OUT"
        );
    };
    let lines: Vec<String> =
        serde_json::from_str(&std::fs::read_to_string(corpus).expect("corpus")).expect("a list");
    let project = PathBuf::from(project);
    let mut output = String::new();
    for line in &lines {
        let mut decisions = serde_json::Map::new();
        for (name, rules) in configurations() {
            for answers in [false, true] {
                let mut engine = PermissionEngine::build_defaults();
                engine.set_workspace(&project);
                for rule in &rules {
                    engine.add_rule(rule.clone());
                }
                if answers {
                    for grant in remembered() {
                        engine.add_grant(grant);
                    }
                }
                let key = if answers {
                    format!("{name}+always")
                } else {
                    name.to_owned()
                };
                decisions.insert(key, decide(&engine, line, &project));
            }
        }
        output.push_str(
            &json!({
                "line": line,
                "reading": reading(line, &project),
                "decisions": decisions,
            })
            .to_string(),
        );
        output.push('\n');
    }
    std::fs::write(out, output).expect("output");
}

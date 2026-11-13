//! Tool calls that no result answers.
//!
//! A call is stored when the model makes it and its result when it ends, so a
//! call can be left without one: copied into a fork while it waited or ran,
//! or cut off when Haarness stopped mid-call. Providers refuse a request that
//! holds such a call, every request after it, so the store and the request
//! builder both find them here.

use std::collections::HashMap;

use haarness_protocol::ItemData;

/// The result a call that will never finish gets, so the model knows it
/// ended and the provider takes the history: stored by startup recovery for
/// the calls a restart cut off, and given in requests to such a call stored
/// without one.
pub const UNFINISHED_CALL_RESULT: &str = "This call did not finish, so there is no result. It may have done part of its work before it stopped.";

/// The metadata of a result whose call never started (its round broke off
/// before it ran, Stop came before it, a batch stopped before reaching it):
/// nothing it would have loaded was, so a rebuild of the chat's loads skips
/// it (see `tool_catalog::promoted_in`).
#[must_use]
pub fn not_run_metadata() -> serde_json::Value {
    serde_json::json!({ "notRun": true })
}

/// Whether a result's metadata says its call never started.
#[must_use]
pub fn not_run(metadata: &serde_json::Value) -> bool {
    metadata.get("notRun").and_then(serde_json::Value::as_bool) == Some(true)
}

/// The stored result of a call Haarness stopped before it finished (startup
/// recovery after a crash, or a shutdown the call outlived). `unfinished` in
/// its metadata lets clients say so in a few words of their own; the model
/// reads [`UNFINISHED_CALL_RESULT`]. A call that had not `started` is marked
/// as not run too.
#[must_use]
pub fn unfinished_call_result(call_id: &str, name: &str, started: bool) -> ItemData {
    let mut metadata = serde_json::json!({ "unfinished": true });
    if !started {
        metadata["notRun"] = true.into();
    }
    ItemData::ToolResult {
        call_id: call_id.to_owned(),
        name: name.to_owned(),
        output: UNFINISHED_CALL_RESULT.to_owned(),
        is_error: true,
        metadata,
    }
}

/// How the calls and results among some items pair up, by position.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ToolCallPairs {
    /// Each call and the result that answers it, in call order.
    pub answered: Vec<(usize, usize)>,
    /// Each call no later result answers, in order.
    pub calls: Vec<usize>,
    /// What was recorded for those calls while they ran: a task's link and
    /// checkpoints, a command's checkpoints. In order.
    pub records: Vec<usize>,
    /// Results that answer no call before them: a second result for a call
    /// already answered, or one whose call is not among the items.
    pub stray: Vec<usize>,
}

/// Pairs the calls in `items` (in order) with their results. A result
/// answers the newest call before it with its id that has none yet: some
/// providers number their calls afresh each turn, and a server may repeat an
/// id within one batch.
#[must_use]
pub fn pair_tool_calls<'a>(items: impl IntoIterator<Item = &'a ItemData>) -> ToolCallPairs {
    let mut open = HashMap::<&str, Vec<(usize, Vec<usize>)>>::new();
    let mut pairs = ToolCallPairs::default();
    for (index, data) in items.into_iter().enumerate() {
        match data {
            ItemData::ToolCall { call_id, .. } => {
                open.entry(call_id).or_default().push((index, Vec::new()));
            }
            ItemData::ToolResult { call_id, .. } => {
                match open.get_mut(call_id.as_str()).and_then(Vec::pop) {
                    Some((call, _)) => pairs.answered.push((call, index)),
                    None => pairs.stray.push(index),
                }
            }
            ItemData::TaskLink { call_id, .. }
            | ItemData::TaskState { call_id, .. }
            | ItemData::CommandState { call_id, .. } => {
                if let Some((_, records)) = open
                    .get_mut(call_id.as_str())
                    .and_then(|calls| calls.last_mut())
                {
                    records.push(index);
                }
            }
            _ => {}
        }
    }
    for (call, records) in open.into_values().flatten() {
        pairs.calls.push(call);
        pairs.records.extend(records);
    }
    pairs.answered.sort_unstable();
    pairs.calls.sort_unstable();
    pairs.records.sort_unstable();
    pairs
}

#[cfg(test)]
mod tests {
    use super::*;
    use haarness_protocol::ThreadId;
    use serde_json::{Value, json};

    fn call(id: &str) -> ItemData {
        ItemData::ToolCall {
            call_id: id.into(),
            name: "bash".into(),
            arguments: json!({"command": "sleep 120"}),
            provider_metadata: Value::Null,
        }
    }

    fn result(id: &str) -> ItemData {
        ItemData::ToolResult {
            call_id: id.into(),
            name: "bash".into(),
            output: "done".into(),
            is_error: false,
            metadata: Value::Null,
        }
    }

    fn text(value: &str) -> ItemData {
        ItemData::AssistantMessage {
            text: value.into(),
            note: false,
        }
    }

    #[test]
    fn a_call_answered_later_is_not_unanswered() {
        let items = [call("a"), call("b"), result("b"), result("a")];
        let pairs = pair_tool_calls(&items);
        assert_eq!(pairs.answered, [(0, 3), (1, 2)]);
        assert!(pairs.calls.is_empty() && pairs.stray.is_empty());
    }

    #[test]
    fn a_call_without_a_result_is_found_with_what_was_recorded_for_it() {
        let child = ThreadId::new();
        let items = [
            call("a"),
            result("a"),
            text("now a subagent"),
            call("task"),
            ItemData::TaskLink {
                call_id: "task".into(),
                child_thread_id: child,
            },
            call("b"),
        ];
        assert_eq!(
            pair_tool_calls(&items),
            ToolCallPairs {
                answered: vec![(0, 1)],
                calls: vec![3, 5],
                records: vec![4],
                stray: Vec::new(),
            }
        );
    }

    #[test]
    fn a_result_answers_only_the_newest_call_with_its_id() {
        // The same id in a later turn, answered there: the first call still
        // has no result.
        let items = [
            call("call-1"),
            text("next turn"),
            call("call-1"),
            result("call-1"),
        ];
        let pairs = pair_tool_calls(&items);
        assert_eq!(pairs.calls, [0]);
        assert_eq!(pairs.answered, [(2, 3)]);
    }

    #[test]
    fn calls_of_one_batch_with_the_same_id_take_a_result_each() {
        let items = [call("x"), call("x"), result("x"), result("x")];
        let pairs = pair_tool_calls(&items);
        assert_eq!(pairs.answered, [(0, 3), (1, 2)]);
        assert!(
            pairs.calls.is_empty() && pairs.stray.is_empty(),
            "{pairs:?}"
        );
    }

    #[test]
    fn a_second_result_for_a_call_is_stray() {
        let items = [result("early"), call("a"), result("a"), result("a")];
        let pairs = pair_tool_calls(&items);
        assert_eq!(pairs.answered, [(1, 2)]);
        assert_eq!(pairs.stray, [0, 3]);
    }

    #[test]
    fn a_record_made_after_the_result_belongs_to_an_answered_call() {
        let child = ThreadId::new();
        let items = [
            call("task"),
            result("task"),
            ItemData::TaskLink {
                call_id: "task".into(),
                child_thread_id: child,
            },
        ];
        let pairs = pair_tool_calls(&items);
        assert!(
            pairs.calls.is_empty() && pairs.records.is_empty(),
            "{pairs:?}"
        );
    }
}

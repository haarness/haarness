//! Tool calls that no result answers.
//!
//! A call is stored when the model makes it and its result when it ends, so a
//! call can be left without one: copied into a fork while it waited or ran,
//! or cut off when Haarness stopped mid-call. Providers refuse a request that
//! holds such a call, every request after it, so the store and the request
//! builder both find them here.

use std::collections::HashMap;

use haarness_protocol::ItemData;

/// The result a call that will never finish gets in requests, so the model
/// knows it ended and the provider takes the history.
pub const UNFINISHED_CALL_RESULT: &str = "This call did not finish, so there is no result. It may have done part of its work before it stopped.";

/// Where the calls without a result are, by position in the items given.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct UnansweredCalls {
    /// Each call no later result answers, in order.
    pub calls: Vec<usize>,
    /// What was recorded for those calls while they ran: a task's link and
    /// checkpoints, a command's checkpoints. In order.
    pub records: Vec<usize>,
}

/// Finds the calls in `items` (in order) that no later result answers. A
/// result answers the newest call before it with its id: some providers
/// number their calls afresh each turn, so an id can come back.
#[must_use]
pub fn unanswered_calls<'a>(items: impl IntoIterator<Item = &'a ItemData>) -> UnansweredCalls {
    let mut open = HashMap::<&str, (usize, Vec<usize>)>::new();
    let mut unanswered = UnansweredCalls::default();
    for (index, data) in items.into_iter().enumerate() {
        match data {
            ItemData::ToolCall { call_id, .. } => {
                if let Some((call, records)) = open.insert(call_id, (index, Vec::new())) {
                    unanswered.calls.push(call);
                    unanswered.records.extend(records);
                }
            }
            ItemData::ToolResult { call_id, .. } => {
                open.remove(call_id.as_str());
            }
            ItemData::TaskLink { call_id, .. }
            | ItemData::TaskState { call_id, .. }
            | ItemData::CommandState { call_id, .. } => {
                if let Some((_, records)) = open.get_mut(call_id.as_str()) {
                    records.push(index);
                }
            }
            _ => {}
        }
    }
    for (call, records) in open.into_values() {
        unanswered.calls.push(call);
        unanswered.records.extend(records);
    }
    unanswered.calls.sort_unstable();
    unanswered.records.sort_unstable();
    unanswered
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
        ItemData::AssistantMessage { text: value.into() }
    }

    #[test]
    fn a_call_answered_later_is_not_unanswered() {
        let items = [call("a"), call("b"), result("b"), result("a")];
        assert_eq!(unanswered_calls(&items), UnansweredCalls::default());
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
            unanswered_calls(&items),
            UnansweredCalls {
                calls: vec![3, 5],
                records: vec![4],
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
        assert_eq!(unanswered_calls(&items).calls, [0]);
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
        assert_eq!(unanswered_calls(&items), UnansweredCalls::default());
    }
}

//! `session_manage search`: every chat in scope, most recently active first,
//! read until a time bound, each match a short excerpt around the hit.

use super::{core_error, item_visible_text, limit, optional_string, required_string};
use anyhow::{Context as _, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use haarness_core::{AgentRuntime, CoreError, ToolContext};
use haarness_protocol::{Item, ItemData, Thread};
use serde_json::{Value, json};
use std::time::Duration;

/// How long one search reads chats before it answers with what it found
/// and how far it got.
pub(super) const SEARCH_TIME: Duration = Duration::from_secs(15);
/// About how many characters of an item a match shows, the hit among them.
const EXCERPT_CHARS: usize = 300;
/// How many of those come before the hit.
const EXCERPT_LEAD: usize = 100;
/// Bytes of matches (compact JSON, chat summaries counted) past which a
/// search stops adding them, so that the answer, laid out, stays well
/// within the 50 KiB a tool's text reaches the model whole.
const ANSWER_BYTES: usize = 32 * 1024;

enum Stop {
    Time,
    Limit,
    Size,
}

/// Searches the visible text of the chats `input` asks for (`query`,
/// `includeArchived`, `before`, `limit`), most recently active first, for
/// at most `time`. The calling chat's own `session_manage` calls, which hold
/// the query, are left out.
pub(super) async fn search(
    runtime: &AgentRuntime,
    input: &Value,
    context: &ToolContext,
    time: Duration,
) -> Result<Value> {
    let started = tokio::time::Instant::now();
    let needle = required_string(input, "query")?.to_ascii_lowercase();
    let before = optional_string(input, "before")
        .map(|text| {
            DateTime::parse_from_rfc3339(text.trim())
                .map(|time| time.with_timezone(&Utc))
                .with_context(|| format!("before '{text}' is not an RFC 3339 time"))
        })
        .transpose()?;
    let include_archived = input
        .get("includeArchived")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let limit = limit(input);
    let mut threads = runtime
        .list_threads(include_archived)
        .await
        .map_err(core_error)?;
    threads.retain(|thread| before.is_none_or(|before| thread.updated_at < before));
    threads.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
    let fragment = stored_fragment(&needle);

    let mut chats = Vec::new();
    let (mut found, mut size, mut searched) = (0, 0, 0);
    // The last chat read to its end: every chat before it was too.
    let mut through: Option<&Thread> = None;
    let mut stop = None;
    for thread in &threads {
        if context.cancel.is_cancelled() {
            return Err(CoreError::Cancelled.into());
        }
        if started.elapsed() >= time {
            stop = Some(Stop::Time);
            break;
        }
        // The store answers at once: let a Stop, and other work, in.
        tokio::task::yield_now().await;
        let own = thread.id == context.thread_id;
        let mut matches = Vec::new();
        let mut summary = None;
        let mut whole = true;
        for item in runtime
            .items_holding(thread.id, fragment)
            .await
            .map_err(core_error)?
        {
            let Some(hit) = item_match(&item, &needle, own) else {
                continue;
            };
            if stop.is_some() {
                // A match past where the search stopped: this chat was
                // not read to its end after all.
                whole = false;
                break;
            }
            let summary = summary.get_or_insert_with(|| super::thread_summary(thread.clone()));
            let bytes = hit.to_string().len()
                + if matches.is_empty() {
                    summary.to_string().len()
                } else {
                    0
                };
            if size + bytes > ANSWER_BYTES {
                stop = Some(Stop::Size);
                whole = false;
                break;
            }
            size += bytes;
            found += 1;
            matches.push(hit);
            if found >= limit {
                // Read on through this chat only, to tell whether it holds
                // more.
                stop = Some(Stop::Limit);
            }
        }
        if !matches.is_empty() {
            chats.push(json!({"thread": summary, "matches": matches}));
        }
        if whole {
            searched += 1;
            through = Some(thread);
        }
        if stop.is_some() {
            break;
        }
    }

    let mut result = json!({
        "chats": chats,
        "searched": {"chats": searched, "of": threads.len()},
    });
    if let Some(stop) = stop {
        let reason = match stop {
            Stop::Time => format!("Stopped after {} s", time.as_secs()),
            Stop::Limit => format!("Stopped at the limit of {limit} matches"),
            Stop::Size => "Stopped when the matches filled the answer".to_owned(),
        };
        let note = match through {
            // A search that stops at its limit with nothing left unread in
            // the chats in scope missed nothing.
            Some(thread) if searched < threads.len() => {
                let time = thread
                    .updated_at
                    .to_rfc3339_opts(SecondsFormat::AutoSi, true);
                result["searched"]["through"] = json!(time);
                format!(
                    "{reason}, with {searched} of {} chats searched. Chats last active before {time} \
                     were not searched: search again with \"before\": \"{time}\" to go on.",
                    threads.len()
                )
            }
            Some(_) => String::new(),
            None if matches!(stop, Stop::Time) => {
                format!("{reason}, before any chat was searched.")
            }
            None => format!(
                "{reason}, before the first chat was searched to its end: narrow the query, or \
                 read that chat with \"read\"."
            ),
        };
        if !note.is_empty() {
            result["stopped"] = json!(note);
        }
    }
    Ok(result)
}

/// A match of `needle` in `item`'s visible text: where it is, what kind of
/// item it is, and about [`EXCERPT_CHARS`] characters around the hit, never
/// the whole item. `None` for no match, and for a `session_manage` call of
/// the calling chat (`own`).
fn item_match(item: &Item, needle: &str, own: bool) -> Option<Value> {
    let (kind, tool) = match &item.data {
        ItemData::UserMessage { .. } => ("user_message", None),
        ItemData::AssistantMessage { .. } => ("assistant_message", None),
        ItemData::Reasoning { .. } => ("reasoning", None),
        ItemData::ToolCall { name, .. } if own && name == "session_manage" => return None,
        ItemData::ToolCall { name, .. } => ("tool_call", Some(name)),
        ItemData::Error { .. } => ("error", None),
        _ => return None,
    };
    let text = item_visible_text(&item.data)?;
    // ASCII case folding keeps every byte where it was.
    let at = text.to_ascii_lowercase().find(needle)?;
    let mut hit = json!({"sequence": item.sequence, "kind": kind});
    if let Some(tool) = tool {
        hit["tool"] = json!(tool);
    }
    hit["excerpt"] = json!(excerpt(&text, at));
    Some(hit)
}

/// About [`EXCERPT_CHARS`] characters of `text` from [`EXCERPT_LEAD`]
/// before byte `at`, with `…` where it was cut.
fn excerpt(text: &str, at: usize) -> String {
    let start = text[..at]
        .char_indices()
        .rev()
        .nth(EXCERPT_LEAD - 1)
        .map_or(0, |(index, _)| index);
    let end = text[start..]
        .char_indices()
        .nth(EXCERPT_CHARS)
        .map_or(text.len(), |(index, _)| start + index);
    let mut shown = String::with_capacity(end - start + 6);
    if start > 0 {
        shown.push('…');
    }
    shown.push_str(&text[start..end]);
    if end < text.len() {
        shown.push('…');
    }
    shown
}

/// The longest run of `query` that every item showing it stores as it is:
/// none with a space in it (a call's name and arguments are stored apart),
/// nor a double quote, a backslash or a control character (JSON writes those
/// in text as escapes). Empty, which narrows nothing, when there is none.
fn stored_fragment(query: &str) -> &str {
    query
        .split(|character: char| {
            character.is_whitespace() || character.is_control() || matches!(character, '"' | '\\')
        })
        .max_by_key(|piece| piece.len())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use haarness_core::{EmptyToolRegistry, EventBus, StaticProviderRegistry, ThreadRepository};
    use haarness_protocol::{CreateThreadRequest, ThreadId, TurnId};
    use haarness_server::ManagementRegistry as _;
    use haarness_store::SqliteStore;
    use std::sync::Arc;
    use tokio_util::sync::CancellationToken;

    struct Chats {
        store: Arc<SqliteStore>,
        runtime: Arc<AgentRuntime>,
    }

    impl Chats {
        fn new() -> Self {
            let store = Arc::new(SqliteStore::in_memory().unwrap());
            let runtime = Arc::new(AgentRuntime::new(
                store.clone(),
                Arc::new(StaticProviderRegistry::new(Vec::new())),
                Arc::new(EmptyToolRegistry),
                EventBus::new(8),
            ));
            Self { store, runtime }
        }

        /// A chat holding `items`, now the most recently active one.
        async fn chat(&self, title: &str, items: Vec<ItemData>) -> ThreadId {
            // Each chat is active after the one before it.
            tokio::time::sleep(Duration::from_millis(2)).await;
            let thread = self
                .store
                .create_thread(CreateThreadRequest {
                    title: Some(title.into()),
                    provider: Some("fixture".into()),
                    model: Some("fixture-1".into()),
                    ..Default::default()
                })
                .await
                .unwrap();
            self.store
                .append_items(thread.id, None, items)
                .await
                .unwrap();
            thread.id
        }

        async fn search(&self, input: Value, from: ThreadId, time: Duration) -> Value {
            search(&self.runtime, &input, &context(from), time)
                .await
                .unwrap()
        }
    }

    fn context(thread: ThreadId) -> ToolContext {
        ToolContext {
            execution: haarness_core::ExecutionSettings::default(),
            thread_id: thread,
            turn_id: TurnId::new(),
            workspace: std::env::temp_dir(),
            authorized_external_paths: Vec::new(),
            cancel: CancellationToken::new(),
            invoker: None,
        }
    }

    fn said(text: &str) -> ItemData {
        ItemData::UserMessage {
            text: text.into(),
            attachments: Vec::new(),
            queued: false,
            synthetic: false,
            command_line: None,
            hook: None,
        }
    }

    fn call(name: &str, arguments: Value) -> ItemData {
        ItemData::ToolCall {
            call_id: format!("call-{}", uuid::Uuid::now_v7()),
            name: name.into(),
            arguments,
            provider_metadata: Value::Null,
        }
    }

    fn titles(result: &Value) -> Vec<&str> {
        result["chats"]
            .as_array()
            .unwrap()
            .iter()
            .map(|chat| chat["thread"]["title"].as_str().unwrap())
            .collect()
    }

    #[tokio::test]
    async fn a_search_reads_every_chat_not_only_the_most_recent() {
        let chats = Chats::new();
        chats
            .chat("oldest", vec![said("the invoice rounding bug")])
            .await;
        let mut here = None;
        for index in 0..205 {
            here = Some(
                chats
                    .chat(&format!("chat {index}"), vec![said("nothing to see")])
                    .await,
            );
        }
        let here = here.unwrap();
        let result = chats
            .search(
                json!({"action": "search", "query": "Invoice Rounding"}),
                here,
                SEARCH_TIME,
            )
            .await;
        assert_eq!(titles(&result), ["oldest"], "{result}");
        assert_eq!(result["searched"], json!({"chats": 206, "of": 206}));
        assert!(result.get("stopped").is_none(), "{result}");
        // Out of time before the first chat: nothing searched, and said so.
        let result = chats
            .search(
                json!({"action": "search", "query": "invoice"}),
                here,
                Duration::ZERO,
            )
            .await;
        assert_eq!(result["chats"], json!([]));
        assert_eq!(result["searched"], json!({"chats": 0, "of": 206}));
        assert!(
            result["stopped"]
                .as_str()
                .unwrap()
                .contains("before any chat was searched"),
            "{result}"
        );
    }

    #[tokio::test]
    async fn a_search_that_stops_says_where_to_go_on_and_going_on_finds_the_rest() {
        let chats = Chats::new();
        chats.chat("first", vec![said("deploy key rotated")]).await;
        chats.chat("second", vec![said("deploy key again")]).await;
        chats.chat("quiet", vec![said("lunch")]).await;
        let third = chats
            .chat(
                "third",
                vec![said("deploy key one"), said("deploy key two")],
            )
            .await;
        let query = |extra: Value| {
            let mut input = json!({"action": "search", "query": "deploy key", "limit": 2});
            input
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            input
        };
        // The limit is reached at the end of the newest chat: it was read to
        // its end, and the older ones were not.
        let result = chats.search(query(json!({})), third, SEARCH_TIME).await;
        assert_eq!(titles(&result), ["third"]);
        assert_eq!(result["searched"]["chats"], 1);
        assert_eq!(result["searched"]["of"], 4);
        let through = result["searched"]["through"].as_str().unwrap().to_owned();
        let stopped = result["stopped"].as_str().unwrap();
        assert!(
            stopped.starts_with("Stopped at the limit of 2 matches, with 1 of 4 chats searched."),
            "{stopped}"
        );
        assert!(
            stopped.contains(&format!("\"before\": \"{through}\"")),
            "{stopped}"
        );
        // Going on from there finds the older ones, and nothing twice.
        let result = chats
            .search(query(json!({"before": through})), third, SEARCH_TIME)
            .await;
        assert_eq!(titles(&result), ["second", "first"], "{result}");
        assert_eq!(result["searched"], json!({"chats": 3, "of": 3}));
        assert!(result.get("stopped").is_none(), "{result}");
        // A limit reached inside a chat leaves that chat to the next search.
        let result = chats
            .search(
                json!({"action": "search", "query": "deploy key", "limit": 1}),
                third,
                SEARCH_TIME,
            )
            .await;
        assert_eq!(result["searched"]["chats"], 0);
        assert!(
            result["stopped"]
                .as_str()
                .unwrap()
                .contains("before the first chat was searched to its end"),
            "{result}"
        );
        // A bad time is refused, naming the field.
        let error = search(
            &chats.runtime,
            &query(json!({"before": "yesterday"})),
            &context(third),
            SEARCH_TIME,
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(error.contains("before 'yesterday'"), "{error}");
    }

    #[tokio::test]
    async fn a_search_finds_text_however_it_is_stored_and_leaves_out_its_own_call() {
        let chats = Chats::new();
        chats
            .chat(
                "other",
                vec![
                    call(
                        "session_manage",
                        json!({"action": "search", "query": "zebra"}),
                    ),
                    said("He said \"Zebra\\crossing\"\nthen left"),
                    call("bash", json!({"command": "ls zebra-folder"})),
                    said("Café ZEBRA"),
                ],
            )
            .await;
        let here = chats
            .chat(
                "here",
                vec![call(
                    "session_manage",
                    json!({"action": "search", "query": "zebra"}),
                )],
            )
            .await;
        let kinds =
            |result: &Value| -> Vec<(String, String)> {
                result["chats"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .flat_map(|chat| {
                        let title = chat["thread"]["title"].as_str().unwrap().to_owned();
                        chat["matches"].as_array().unwrap().iter().map(move |hit| {
                            (title.clone(), hit["kind"].as_str().unwrap().to_owned())
                        })
                    })
                    .collect()
            };
        let run = |query: &str| {
            let input = json!({"action": "search", "query": query});
            let chats = &chats;
            async move { chats.search(input, here, SEARCH_TIME).await }
        };
        // Its own call holds the query; another chat's call is found.
        let result = run("zebra").await;
        assert_eq!(
            kinds(&result),
            [
                ("other".to_owned(), "tool_call".to_owned()),
                ("other".to_owned(), "user_message".to_owned()),
                ("other".to_owned(), "tool_call".to_owned()),
                ("other".to_owned(), "user_message".to_owned()),
            ],
            "{result}"
        );
        assert_eq!(result["chats"][0]["matches"][2]["tool"], "bash");
        // Text JSON stores escaped, a call's name and arguments together,
        // and letters in another ASCII case.
        for query in [
            "said \"zebra\\crossing\"\nthen",
            "bash {\"command\":\"ls zebra",
            "h {\"command\"",
            "café zebra",
        ] {
            let result = run(query).await;
            assert_eq!(
                result["chats"][0]["matches"].as_array().map(Vec::len),
                Some(1),
                "{query:?}: {result}"
            );
        }
        // Folding is ASCII's only, as before.
        assert_eq!(run("CAFÉ").await["chats"], json!([]));
    }

    #[tokio::test]
    async fn each_match_is_an_excerpt_and_the_answer_stays_small() {
        let chats = Chats::new();
        let content = format!(
            "{}needle in the haystack{}",
            "a".repeat(150_000),
            "b".repeat(150_000)
        );
        let mut here = None;
        for index in 0..60 {
            here = Some(
                chats
                    .chat(
                        &format!("chat {index}"),
                        vec![call(
                            "write",
                            json!({"path": "big.txt", "content": content}),
                        )],
                    )
                    .await,
            );
        }
        let (root, _workspace, _paths, service) =
            crate::management::tests::isolated_service("session-search-size");
        service.attach_runtime(&chats.runtime);
        let output = service
            .execute(
                "session_manage",
                json!({"action": "search", "query": "needle", "limit": 100}),
                context(here.unwrap()),
            )
            .await
            .unwrap();
        assert!(output.text.len() < 50 * 1024, "{}", output.text.len());
        let result: Value = serde_json::from_str(&output.text).unwrap();
        let first = &result["chats"][0]["matches"][0];
        let excerpt = first["excerpt"].as_str().unwrap();
        assert!(excerpt.contains("needle in the haystack"), "{excerpt}");
        assert!(
            excerpt.starts_with('…') && excerpt.ends_with('…'),
            "{excerpt}"
        );
        assert_eq!(excerpt.chars().count(), EXCERPT_CHARS + 2);
        assert_eq!(first["tool"], "write");
        assert!(
            result["stopped"]
                .as_str()
                .unwrap()
                .starts_with("Stopped when the matches filled the answer"),
            "{result}"
        );
        // The metadata says how far it got without the matches again.
        let metadata = &output.metadata["management"];
        assert!(metadata.get("chats").is_none(), "{metadata}");
        assert_eq!(metadata["searched"]["of"], 60);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_stopped_turn_stops_the_search() {
        let chats = Chats::new();
        let here = chats.chat("here", vec![said("anything")]).await;
        let context = context(here);
        context.cancel.cancel();
        let error = search(
            &chats.runtime,
            &json!({"action": "search", "query": "anything"}),
            &context,
            SEARCH_TIME,
        )
        .await
        .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<CoreError>(),
            Some(CoreError::Cancelled)
        ));
    }

    #[test]
    fn an_excerpt_is_cut_on_characters_around_the_hit() {
        assert_eq!(excerpt("short text", 6), "short text");
        let text = format!("{}é hit {}", "ü".repeat(500), "ß".repeat(500));
        let at = text.find("hit").unwrap();
        let shown = excerpt(&text, at);
        assert!(shown.starts_with('…') && shown.ends_with('…'));
        assert_eq!(shown.chars().count(), EXCERPT_CHARS + 2);
        assert_eq!(shown.chars().position(|c| c == 'h'), Some(EXCERPT_LEAD + 1));
    }

    #[test]
    fn the_stored_fragment_is_one_every_item_showing_the_query_stores() {
        assert_eq!(stored_fragment("deploy key"), "deploy");
        assert_eq!(stored_fragment("bash {\"command\":\"ls"), "command");
        assert_eq!(stored_fragment("a\nbcd"), "bcd");
        assert_eq!(stored_fragment(" "), "");
        assert_eq!(stored_fragment(""), "");
    }
}

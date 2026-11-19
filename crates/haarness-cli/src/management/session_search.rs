//! `session_manage search`: every chat in scope, most recently active first,
//! read until a time bound, each match a short excerpt around the hit.

use super::{core_error, item_visible_text, limit, optional_string, required_string};
use anyhow::{Context as _, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use haarness_core::{AgentRuntime, CoreError, ToolContext};
use haarness_protocol::{Item, ItemData, Thread, ThreadId};
use serde_json::{Value, json};
use std::{fmt::Write as _, time::Duration};
use uuid::Uuid;

/// How long one search reads chats before it answers with what it found
/// and how far it got.
pub(super) const SEARCH_TIME: Duration = Duration::from_secs(15);
/// About how many characters of an item a match shows, the hit among them.
const EXCERPT_CHARS: usize = 300;
/// How many of those come before the hit.
const EXCERPT_LEAD: usize = 100;
/// The most bytes a search's chats and matches take in its answer, laid out
/// as the answer is; what it says about how far it got comes on top.
const ANSWER_BYTES: usize = 46 * 1024;

enum Stop {
    Time,
    Limit,
    Size,
}

/// Where a search goes on from: the chats after a point in the order a run
/// of searches reads them, given as `"<time> <chat id> <run's start>"`. A
/// bare time stands for the chats last active before it.
struct Cursor {
    time: DateTime<Utc>,
    id: Option<Uuid>,
    /// When the run's first search began.
    since: Option<DateTime<Utc>>,
}

impl Cursor {
    fn parse(text: &str) -> Result<Self> {
        let refused = || {
            format!(
                "before '{text}' is not an RFC 3339 time, alone or as a search that stopped gave it"
            )
        };
        let time = |word: &str| {
            DateTime::parse_from_rfc3339(word)
                .map(|time| time.with_timezone(&Utc))
                .with_context(refused)
        };
        let mut words = text.split_whitespace();
        let at = time(words.next().unwrap_or_default())?;
        let id = words
            .next()
            .map(|id| Uuid::parse_str(id).with_context(refused))
            .transpose()?;
        let since = words.next().map(time).transpose()?;
        if words.next().is_some() {
            anyhow::bail!(refused());
        }
        Ok(Self {
            time: at,
            id,
            since,
        })
    }

    /// The point just after the chat at `place`, in a run that began at
    /// `since`.
    fn after((at, id): Place, since: DateTime<Utc>) -> String {
        let written = |time: DateTime<Utc>| time.to_rfc3339_opts(SecondsFormat::AutoSi, true);
        format!("{} {id} {}", written(at), written(since))
    }
}

/// A chat's place in the order a run of searches reads chats, last first.
type Place = (DateTime<Utc>, Uuid);

/// Where `thread` comes in a run of searches that began at `since`: by when
/// it was last active, most recent first, then by id; or, when it has been
/// active since the run began, after every other chat, since it may have
/// moved past the point where the run goes on.
///
/// The calling chat comes first in the run's first search (`since` is
/// `None`), even when another chat saved something between its call and the
/// search, and is left out of the later ones (`None`): each search of it
/// makes it active again, and the first one read it, or stopped inside it
/// and said how to go on there.
fn place(thread: &Thread, since: Option<DateTime<Utc>>, calling: ThreadId) -> Option<Place> {
    if thread.id == calling {
        return since.is_none().then_some((first_place(), thread.id.0));
    }
    match since {
        Some(since) if thread.updated_at > since => Some((DateTime::UNIX_EPOCH, thread.id.0)),
        _ => Some((thread.updated_at, thread.id.0)),
    }
}

/// A time later than any chat's, which a cursor still writes and reads back
/// in RFC 3339 (9999-12-31T23:59:59Z).
fn first_place() -> DateTime<Utc> {
    DateTime::from_timestamp(253_402_300_799, 0).unwrap_or(DateTime::<Utc>::MAX_UTC)
}

/// Searches the visible text of the chats `input` asks for (`query`,
/// `includeArchived`, `before`, `limit`), most recently active first, for
/// at most `time`; or, given `only`, that one chat's items after `after`.
/// The calling chat's own session searches, which hold the query, are left
/// out.
pub(super) async fn search(
    runtime: &AgentRuntime,
    input: &Value,
    context: &ToolContext,
    only: Option<Thread>,
    time: Duration,
) -> Result<Value> {
    let started = tokio::time::Instant::now();
    let began = Utc::now();
    let needle = required_string(input, "query")?.to_ascii_lowercase();
    let before = optional_string(input, "before")
        .map(Cursor::parse)
        .transpose()?;
    let since = before.as_ref().and_then(|before| before.since);
    let include_archived = input
        .get("includeArchived")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let limit = limit(input);
    let after = only
        .as_ref()
        .and(input.get("after").and_then(Value::as_u64))
        .unwrap_or(0);
    let mut threads = match only {
        Some(thread) => vec![thread],
        None => {
            let mut threads = runtime
                .list_threads(include_archived)
                .await
                .map_err(core_error)?;
            threads.retain(|thread| match &before {
                None => true,
                Some(Cursor { time, id: None, .. }) => thread.updated_at < *time,
                Some(Cursor {
                    time, id: Some(id), ..
                }) => place(thread, since, context.thread_id)
                    .is_some_and(|place| place < (*time, *id)),
            });
            threads
        }
    };
    threads.sort_by_key(|thread| std::cmp::Reverse(place(thread, since, context.thread_id)));
    let fragment = stored_fragment(&needle);

    let mut chats = Vec::new();
    let (mut found, mut size, mut searched) = (0, 0, 0);
    // The last chat read to its end: every chat before it was too.
    let mut through: Option<&Thread> = None;
    // The chat the search stopped inside, and the last item of it read.
    let mut cut: Option<(&Thread, u64)> = None;
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
        let mut read = after;
        for item in runtime
            .items_holding(thread.id, fragment)
            .await
            .map_err(core_error)?
        {
            if item.sequence <= after {
                continue;
            }
            let Some(hit) = item_match(&item, &needle, own) else {
                continue;
            };
            if stop.is_some() {
                // A match past where the search stopped: this chat was
                // not read to its end after all.
                cut = Some((thread, read));
                break;
            }
            let summary = summary.get_or_insert_with(|| super::thread_summary(thread.clone()));
            let bytes = laid_out(&hit, 4)
                + MATCH_FRAME
                + if matches.is_empty() {
                    laid_out(summary, 3) + CHAT_FRAME
                } else {
                    0
                };
            if size + bytes > ANSWER_BYTES {
                stop = Some(Stop::Size);
                cut = Some((thread, read));
                break;
            }
            size += bytes;
            found += 1;
            read = item.sequence;
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
        if cut.is_none() {
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
    let Some(stop) = stop else {
        return Ok(result);
    };
    // The chats after the one the search stopped in, or after the last one
    // it read to its end, are the ones left to search.
    let last = cut.map(|(thread, _)| thread).or(through);
    let rest_of_chats = match last {
        Some(last) => threads
            .last()
            .is_some_and(|oldest| oldest.id != last.id)
            .then(|| {
                place(last, since, context.thread_id)
                    .map(|place| Cursor::after(place, since.unwrap_or(began)))
            })
            .flatten(),
        None => None,
    };
    let reason = match stop {
        Stop::Time => format!("Stopped after {} s", time.as_secs()),
        Stop::Limit => format!(
            "Stopped at the limit of {limit} {}",
            if limit == 1 { "match" } else { "matches" }
        ),
        Stop::Size => "Stopped when the matches filled the answer".to_owned(),
    };
    let mut note = format!(
        "{reason}, with {searched} of {} chats searched.",
        threads.len()
    );
    if let Some((thread, read)) = cut {
        result["restOfChat"] = json!({"threadId": thread.id, "after": read});
        let _ = write!(
            note,
            " Chat {id} was searched up to item {read}: for its later matches, search again \
             with \"threadId\": \"{id}\", \"after\": {read}.",
            id = thread.id
        );
    }
    match rest_of_chats {
        Some(cursor) => {
            result["next"] = json!({"before": cursor});
            let _ = write!(
                note,
                " For {}, search again with \"before\": \"{cursor}\"; chats active since the \
                 first of these searches began come last then, even past a time it was given, \
                 so a chat may come twice.",
                if cut.is_some() {
                    "the chats after it"
                } else {
                    "the chats not searched yet"
                }
            );
        }
        None if last.is_none() && cut.is_none() && !threads.is_empty() => {
            note.push_str(" Search again to go on.");
        }
        None => {}
    }
    // A search that stops at its limit with nothing left unread missed
    // nothing.
    if cut.is_some() || result.get("next").is_some() || last.is_none() {
        result["stopped"] = json!(note);
    }
    Ok(result)
}

/// Bytes a match takes in the answer besides its own laid-out text: its
/// indent, a comma and a line break.
const MATCH_FRAME: usize = 8 + 2;
/// Bytes a chat takes in the answer besides its summary and matches: the
/// braces, indents and keys around them.
const CHAT_FRAME: usize = 64;

/// How many bytes `value` takes laid out as the answer lays it out (serde's
/// two-space indent), `depth` levels in.
fn laid_out(value: &Value, depth: usize) -> usize {
    let text = serde_json::to_string_pretty(value).unwrap_or_default();
    text.len() + text.matches('\n').count() * 2 * depth
}

/// A match of `needle` in `item`'s visible text: where it is, what kind of
/// item it is, and about [`EXCERPT_CHARS`] characters around the hit, never
/// the whole item. `None` for no match, and in the calling chat (`own`) for
/// a call that searched sessions, itself or through `batch`, Code Mode or a
/// workflow, since it holds the query.
fn item_match(item: &Item, needle: &str, own: bool) -> Option<Value> {
    let (kind, tool) = match &item.data {
        ItemData::UserMessage { .. } => ("user_message", None),
        ItemData::AssistantMessage { .. } => ("assistant_message", None),
        ItemData::Reasoning { .. } => ("reasoning", None),
        ItemData::ToolCall {
            name, arguments, ..
        } if own
            && (name == "session_manage"
                || matches!(name.as_str(), "batch" | "execute" | "workflow")
                    && arguments.to_string().contains("session_manage")) =>
        {
            return None;
        }
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
    use std::path::PathBuf;
    use std::sync::Arc;
    use tokio_util::sync::CancellationToken;

    struct Chats {
        store: Arc<SqliteStore>,
        runtime: Arc<AgentRuntime>,
        /// The folder of a store kept in a file, which a test can change
        /// behind the store's back.
        folder: Option<PathBuf>,
    }

    impl Chats {
        fn new() -> Self {
            Self::with(Arc::new(SqliteStore::in_memory().unwrap()), None)
        }

        fn on_disk(name: &str) -> Self {
            let folder =
                std::env::temp_dir().join(format!("haarness-{name}-{}", uuid::Uuid::now_v7()));
            std::fs::create_dir_all(&folder).unwrap();
            let store = Arc::new(SqliteStore::open(folder.join("haarness.db")).unwrap());
            Self::with(store, Some(folder))
        }

        fn with(store: Arc<SqliteStore>, folder: Option<PathBuf>) -> Self {
            let runtime = Arc::new(AgentRuntime::new(
                store.clone(),
                Arc::new(StaticProviderRegistry::new(Vec::new())),
                Arc::new(EmptyToolRegistry),
                EventBus::new(8),
            ));
            Self {
                store,
                runtime,
                folder,
            }
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

        /// Makes `chats` last active at one moment, as a restart that
        /// recovers their turns does.
        fn at_one_moment(&self, chats: &[ThreadId]) {
            let folder = self.folder.as_ref().expect("a store in a file");
            let connection = rusqlite::Connection::open(folder.join("haarness.db")).unwrap();
            let moment = Utc::now().to_rfc3339();
            for chat in chats {
                connection
                    .execute(
                        "UPDATE threads SET updated_at=?1 WHERE id=?2",
                        rusqlite::params![moment, chat.to_string()],
                    )
                    .unwrap();
            }
        }

        async fn search(&self, input: Value, from: ThreadId, time: Duration) -> Value {
            self.search_in(input, from, None, time).await
        }

        async fn search_in(
            &self,
            input: Value,
            from: ThreadId,
            only: Option<ThreadId>,
            time: Duration,
        ) -> Value {
            let only = match only {
                Some(id) => Some(self.runtime.get_thread(id).await.unwrap()),
                None => None,
            };
            search(&self.runtime, &input, &context(from), only, time)
                .await
                .unwrap()
        }
    }

    impl Drop for Chats {
        fn drop(&mut self) {
            if let Some(folder) = &self.folder {
                let _ = std::fs::remove_dir_all(folder);
            }
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

    fn with(mut input: Value, extra: Value) -> Value {
        input
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        input
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
                .starts_with("Stopped after 0 s, with 0 of 206 chats searched."),
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
                vec![
                    said("deploy key one"),
                    said("lunch"),
                    said("deploy key two"),
                ],
            )
            .await;
        let query = |extra: Value| {
            with(
                json!({"action": "search", "query": "deploy key", "limit": 2}),
                extra,
            )
        };
        // The limit is reached at the end of the newest chat: it was read to
        // its end, and the older ones were not.
        let result = chats.search(query(json!({})), third, SEARCH_TIME).await;
        assert_eq!(titles(&result), ["third"]);
        assert_eq!(result["searched"], json!({"chats": 1, "of": 4}));
        assert!(result.get("restOfChat").is_none(), "{result}");
        let third_time = result["chats"][0]["thread"]["updatedAt"]
            .as_str()
            .unwrap()
            .to_owned();
        let next = result["next"]["before"].as_str().unwrap().to_owned();
        assert!(next.contains(&format!(" {third} ")), "{next}");
        let stopped = result["stopped"].as_str().unwrap();
        assert_eq!(
            stopped,
            format!(
                "Stopped at the limit of 2 matches, with 1 of 4 chats searched. For the chats \
                 not searched yet, search again with \"before\": \"{next}\"; chats active since the \
                 first of these searches began come last then, even past a time it was given, \
                 so a chat may come twice."
            )
        );
        // Going on from there finds the older ones, and nothing twice.
        let result = chats
            .search(query(json!({"before": next})), third, SEARCH_TIME)
            .await;
        assert_eq!(titles(&result), ["second", "first"], "{result}");
        assert_eq!(result["searched"], json!({"chats": 3, "of": 3}));
        assert!(result.get("stopped").is_none(), "{result}");
        // A bare time still reads as the chats last active before it.
        let result = chats
            .search(query(json!({"before": third_time})), third, SEARCH_TIME)
            .await;
        assert_eq!(titles(&result), ["second", "first"], "{result}");

        // A limit reached inside a chat: that chat goes on from the last
        // match given, and the chats after it from after it, so nothing
        // comes twice and a chat with more matches than the limit is no wall.
        let one = |extra: Value| {
            with(
                json!({"action": "search", "query": "deploy key", "limit": 1}),
                extra,
            )
        };
        let result = chats.search(one(json!({})), third, SEARCH_TIME).await;
        assert_eq!(result["searched"], json!({"chats": 0, "of": 4}));
        assert_eq!(result["chats"][0]["matches"][0]["sequence"], 1);
        assert_eq!(
            result["restOfChat"],
            json!({"threadId": third, "after": 1}),
            "{result}"
        );
        let next = result["next"]["before"].as_str().unwrap().to_owned();
        let stopped = result["stopped"].as_str().unwrap();
        assert!(
            stopped.contains(&format!(
                "Chat {third} was searched up to item 1: for its later matches, search again \
                 with \"threadId\": \"{third}\", \"after\": 1. For the chats after it, search \
                 again with \"before\": \"{next}\";"
            )),
            "{stopped}"
        );
        let rest = chats
            .search_in(one(json!({"after": 1})), third, Some(third), SEARCH_TIME)
            .await;
        assert_eq!(rest["chats"][0]["matches"][0]["sequence"], 3, "{rest}");
        assert_eq!(rest["searched"], json!({"chats": 1, "of": 1}));
        assert!(rest.get("stopped").is_none(), "{rest}");
        let older = chats
            .search(one(json!({"before": next})), third, SEARCH_TIME)
            .await;
        assert_eq!(titles(&older), ["second"], "{older}");

        // A bad time is refused, naming the field.
        for before in [
            "yesterday",
            "2026-11-19T03:39:03Z not-an-id",
            "2026-11-19T03:39:03Z 01a1f1f3-4cc0-7592-85e5-0d2f0cf157c9 later",
        ] {
            let error = search(
                &chats.runtime,
                &query(json!({"before": before})),
                &context(third),
                None,
                SEARCH_TIME,
            )
            .await
            .unwrap_err()
            .to_string();
            assert!(error.contains(&format!("before '{before}'")), "{error}");
        }
    }

    #[tokio::test]
    async fn going_on_reaches_a_chat_that_became_active_in_between() {
        let chats = Chats::new();
        let older = chats.chat("older", vec![said("canary release")]).await;
        chats.chat("middle", vec![said("canary release")]).await;
        let here = chats.chat("here", vec![said("canary release")]).await;
        // Another chat saved something between the calling chat's call and
        // the search: the calling chat is still read first.
        chats.chat("busy", vec![said("canary release")]).await;
        let input = |before: Option<&str>| {
            let mut input = json!({"action": "search", "query": "canary release", "limit": 1});
            if let Some(before) = before {
                input["before"] = json!(before);
            }
            input
        };
        let first = chats.search(input(None), here, SEARCH_TIME).await;
        assert_eq!(titles(&first), ["here"]);
        // Before the next call the calling chat saves the search's result,
        // and the oldest chat gets a message: it is now the most recently
        // active, past the point the search gave.
        for chat in [here, older] {
            chats
                .store
                .append_item(chat, None, said("lunch"))
                .await
                .unwrap();
        }
        let mut seen = Vec::new();
        let mut before = first["next"]["before"].as_str().map(str::to_owned);
        for _ in 0..10 {
            let Some(point) = before.take() else { break };
            let result = chats.search(input(Some(&point)), here, SEARCH_TIME).await;
            seen.extend(titles(&result).into_iter().map(str::to_owned));
            before = result["next"]["before"].as_str().map(str::to_owned);
        }
        // Each is reached once, the one active since last, and the calling
        // chat is not read again for its own activity.
        assert_eq!(seen, ["busy", "middle", "older"]);
    }

    #[tokio::test]
    async fn going_on_skips_no_chat_last_active_at_the_same_moment() {
        let chats = Chats::on_disk("session-search-ties");
        let mut tied = Vec::new();
        for title in ["w", "x", "y", "z"] {
            let chat = chats
                .chat(title, vec![said(&format!("rollback plan {title}"))])
                .await;
            if title != "w" {
                tied.push(chat);
            }
        }
        chats.at_one_moment(&tied);
        let here = tied[0];
        let mut seen = Vec::new();
        let mut before = None;
        for _ in 0..10 {
            let mut input = json!({"action": "search", "query": "rollback plan", "limit": 1});
            if let Some(before) = &before {
                input["before"] = json!(before);
            }
            let result = chats.search(input, here, SEARCH_TIME).await;
            assert!(result.get("restOfChat").is_none(), "{result}");
            seen.extend(titles(&result).into_iter().map(str::to_owned));
            match result.get("next") {
                Some(next) => before = Some(next["before"].as_str().unwrap().to_owned()),
                None => break,
            }
        }
        seen.sort();
        assert_eq!(seen, ["w", "x", "y", "z"]);
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
                vec![
                    call(
                        "session_manage",
                        json!({"action": "search", "query": "zebra"}),
                    ),
                    call(
                        "batch",
                        json!({"tool_calls": [{"tool": "session_manage", "parameters": {"action": "search", "query": "zebra"}}]}),
                    ),
                    call(
                        "execute",
                        json!({"code": "return await tools.session_manage({action: 'search', query: 'zebra'})"}),
                    ),
                    // An edit that mentions the tool is no search.
                    call(
                        "edit",
                        json!({"path": "tools.rs", "old": "session_manage", "new": "zebra"}),
                    ),
                ],
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
        // Its own searches hold the query, made directly or through batch
        // or Code Mode; its other calls and another chat's search are found.
        let result = run("zebra").await;
        assert_eq!(
            kinds(&result),
            [
                ("here".to_owned(), "tool_call".to_owned()),
                ("other".to_owned(), "tool_call".to_owned()),
                ("other".to_owned(), "user_message".to_owned()),
                ("other".to_owned(), "tool_call".to_owned()),
                ("other".to_owned(), "user_message".to_owned()),
            ],
            "{result}"
        );
        assert_eq!(result["chats"][0]["matches"][0]["tool"], "edit");
        assert_eq!(result["chats"][1]["matches"][2]["tool"], "bash");
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
        assert!(output.text.len() <= 50 * 1024, "{}", output.text.len());
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
    async fn many_short_matches_fill_the_answer_as_it_is_sent() {
        let chats = Chats::new();
        let mut here = None;
        for index in 0..100 {
            here = Some(
                chats
                    .chat(
                        &format!("Fix the login flow {index}"),
                        vec![said("please deploy")],
                    )
                    .await,
            );
        }
        let (root, _workspace, _paths, service) =
            crate::management::tests::isolated_service("session-search-short");
        service.attach_runtime(&chats.runtime);
        let output = service
            .execute(
                "session_manage",
                json!({"action": "search", "query": "deploy", "limit": 100}),
                context(here.unwrap()),
            )
            .await
            .unwrap();
        // Laid out as sent, the answer stays within what reaches the model
        // whole, and says where to go on.
        assert!(output.text.len() <= 50 * 1024, "{}", output.text.len());
        let result: Value = serde_json::from_str(&output.text).unwrap();
        let shown = result["chats"].as_array().unwrap().len();
        assert!((40..100).contains(&shown), "{shown}");
        assert_eq!(result["searched"]["chats"], shown);
        assert!(result["next"]["before"].is_string(), "{result}");
        assert!(
            result["stopped"]
                .as_str()
                .unwrap()
                .starts_with("Stopped when the matches filled the answer"),
            "{result}"
        );
        // The bytes counted for the chats are no fewer than they take.
        let counted: usize = result["chats"]
            .as_array()
            .unwrap()
            .iter()
            .map(|chat| {
                CHAT_FRAME
                    + laid_out(&chat["thread"], 3)
                    + chat["matches"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|hit| MATCH_FRAME + laid_out(hit, 4))
                        .sum::<usize>()
            })
            .sum();
        let mut bare = result.clone();
        bare["chats"] = json!([]);
        let rest = serde_json::to_string_pretty(&bare).unwrap().len();
        assert!(
            output.text.len() <= counted + rest,
            "{} > {counted} + {rest}",
            output.text.len()
        );
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
            None,
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

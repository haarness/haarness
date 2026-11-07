//! What a compaction keeps verbatim beside its summary.
//!
//! The summary stands for the history it replaces, but the model works from
//! what it sees word for word: above all the request it is answering, and the
//! calls it just made. So every compaction keeps the person's latest request
//! (and whatever it attached, and what else they wrote in its turn), then as
//! much of the newest history as the kept part's budget allows, whole calls
//! and their results together. Whole recent turns come first, as before;
//! when the request's own turn is too long for that, its newest rounds; and
//! when even the newest round is too long, its tool outputs are cut to their
//! start and end, every kept output alike, so the request never is. A
//! request that leaves no room even for itself is refused in plain words
//! rather than summarized away.
//!
//! The kept part is described by sequences, as the transcript stores it: a
//! verbatim tail from one sequence on, the items before it kept all the
//! same, and how many characters each kept tool output shows. Stored items
//! never change; the cut is made where messages are built
//! ([`haarness_protocol::tokens::OutputCut`]), and the estimate that starts
//! compaction counts it too, so a compacted chat never compacts again for
//! the part it kept.

use std::collections::HashSet;

use haarness_protocol::tokens::{self, MIN_KEPT_OUTPUT_CHARS, OutputCut, Tokenizer};
use haarness_protocol::{Item, ItemData, TurnId};

/// Tokens left free under the compaction limit after a compaction, at most:
/// the estimate is close, not exact, and the next round adds to it. A small
/// limit keeps a fiftieth of itself instead.
const MARGIN: u64 = 512;

/// How the person is told their message cannot fit. Turn failures that start
/// with it are shown as they are.
pub(super) const MESSAGE_TOO_LONG: &str =
    "The latest message is too long for this model's context window";

/// Room for what a compaction keeps.
#[derive(Debug, Clone, Copy)]
pub(super) struct Budget {
    /// What the kept part should not pass: the configured share of the
    /// window for recent history.
    pub soft: u64,
    /// What it cannot pass: the limit, less the instructions and tools and
    /// the summary.
    pub hard: u64,
    /// How the provider counts history against the estimate (at least 1, so
    /// that the estimate after the compaction never reads larger).
    pub calibration: f64,
}

impl Budget {
    /// The budget under `input_limit` for a summary of `summary_tokens`,
    /// with `share` of the room (less than 1 after a provider refused a
    /// request the estimate took to fit).
    pub fn new(
        settings: &crate::CompactionSettings,
        items: &[Item],
        input_limit: Option<u64>,
        summary_tokens: u64,
        share: f64,
    ) -> Self {
        let configured = input_limit
            .map(|limit| limit / 2)
            .unwrap_or(settings.preserve_recent_tokens)
            .min(settings.preserve_recent_tokens);
        let hard = input_limit.map_or(u64::MAX, |limit| {
            // The instructions and tools as the estimate that starts
            // compaction counts them: none before a request reported them.
            let overhead = tokens::request_overhead(items).unwrap_or(0);
            let margin = MARGIN.min(limit / 50);
            // A quarter of the limit at least: instructions that leave less
            // are what is too long for this window, not the message.
            let room = limit
                .saturating_sub(overhead + summary_tokens + margin)
                .max(limit / 4);
            #[allow(
                clippy::cast_precision_loss,
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss
            )]
            let shared = (room as f64 * share.clamp(0.0, 1.0)) as u64;
            shared
        });
        Self {
            soft: configured.min(hard),
            hard,
            calibration: tokens::calibration(items).max(1.0),
        }
    }

    fn fits(&self, estimate: u64, target: u64) -> bool {
        #[allow(clippy::cast_precision_loss)]
        let counted = estimate as f64 * self.calibration;
        #[allow(clippy::cast_precision_loss)]
        let target = target as f64;
        counted <= target
    }
}

/// What a compaction keeps: [`ItemData::Compaction`]'s `tail_start_sequence`,
/// `kept_sequences` and `tool_output_chars`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Plan {
    pub tail_start: Option<u64>,
    pub kept: Vec<u64>,
    pub output_chars: Option<u64>,
}

/// The person's latest request does not fit in the room there is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct TooLong {
    pub request: u64,
    pub room: u64,
}

impl TooLong {
    pub fn message(self) -> String {
        format!(
            "{MESSAGE_TOO_LONG}: it takes about {} tokens, and there is room for about {} beside Haarness's instructions and tools. Shorten it or send it in parts, or choose a model with a larger context window.",
            rounded(self.request),
            rounded(self.room)
        )
    }
}

/// `tokens` to two significant figures, its thousands apart: `75,000`.
fn rounded(tokens: u64) -> String {
    let magnitude = 10_u64.pow(tokens.max(1).ilog10().saturating_sub(1));
    tokens::grouped((tokens + magnitude / 2) / magnitude * magnitude)
}

/// A message the person wrote (a steer they sent while a turn ran
/// included), not one Haarness made for them.
fn written(item: &Item) -> bool {
    matches!(
        item.data,
        ItemData::UserMessage {
            synthetic: false,
            ..
        }
    )
}

fn queued(item: &Item) -> bool {
    matches!(item.data, ItemData::UserMessage { queued: true, .. })
}

/// The chat as the model sees it now, without the summary.
fn in_view(items: &[Item]) -> Vec<&Item> {
    tokens::visible_items(items)
        .into_iter()
        .filter(|item| !matches!(item.data, ItemData::Compaction { .. }))
        .collect()
}

/// Positions in `visible` of what the person wrote in the turn of their
/// latest message, that message last; empty when they wrote nothing.
fn request_group(visible: &[&Item]) -> Vec<usize> {
    let Some(latest) = visible.iter().rposition(|item| written(item)) else {
        return Vec::new();
    };
    let turn: Option<TurnId> = visible[latest].turn_id;
    (0..=latest)
        .filter(|&index| {
            written(visible[index])
                && (index == latest || (turn.is_some() && visible[index].turn_id == turn))
        })
        .collect()
}

/// Tokens of the person's latest request, as [`plan`] keeps it, when it
/// cannot fit under `budget`.
pub(super) fn too_long(items: &[Item], budget: Budget, tokenizer: Tokenizer) -> Option<TooLong> {
    let visible = in_view(items);
    let cut = OutputCut::of(items);
    let request = request_group(&visible)
        .into_iter()
        .map(|index| tokenizer.shown(visible[index], cut))
        .sum::<u64>();
    (!budget.fits(request, budget.hard)).then_some(TooLong {
        request,
        room: budget.hard,
    })
}

/// Which positions a tail may start at without parting a call from its
/// result: `starts[s]` for a tail of `visible[s..]` (`starts[len]`, the
/// empty tail, always).
fn tail_starts(visible: &[&Item]) -> Vec<bool> {
    let pairs = crate::pair_tool_calls(visible.iter().map(|item| &item.data));
    let mut crossing = vec![0_i64; visible.len() + 2];
    for (call, result) in pairs.answered {
        crossing[call + 1] += 1;
        crossing[result + 1] -= 1;
    }
    let mut open = 0;
    (0..=visible.len())
        .map(|start| {
            open += crossing[start];
            open == 0
        })
        .collect()
}

/// Token sizes of the items in view, whole or with tool outputs cut.
struct Sizes<'a> {
    visible: &'a [&'a Item],
    whole: Vec<u64>,
    tokenizer: Tokenizer,
    /// The compaction to come, after every item: all kept results are cut.
    before: u64,
}

impl Sizes<'_> {
    fn of(&self, index: usize, chars: Option<u64>) -> u64 {
        match chars {
            None => self.whole[index],
            Some(chars) => self.tokenizer.shown(
                self.visible[index],
                Some(OutputCut {
                    before: self.before,
                    chars,
                }),
            ),
        }
    }

    /// The kept part: `visible[start..]` and the `group` items before it.
    fn kept(&self, start: usize, group: &[usize], chars: Option<u64>) -> u64 {
        let tail = (start..self.visible.len())
            .map(|index| self.of(index, chars))
            .sum::<u64>();
        let hoisted = group
            .iter()
            .filter(|&&index| index < start)
            .map(|&index| self.of(index, chars))
            .sum::<u64>();
        tail + hoisted
    }

    /// The longest output in `visible[start..]`.
    fn longest_output(&self, start: usize) -> u64 {
        self.visible[start..]
            .iter()
            .filter_map(|item| match &item.data {
                ItemData::ToolResult { output, .. } => Some(output.chars().count() as u64),
                _ => None,
            })
            .max()
            .unwrap_or(0)
    }
}

/// What a compaction of `items` (the whole transcript, in sequence order,
/// undone history gone) keeps under `budget`; see the module's account.
pub(super) fn plan(
    items: &[Item],
    tail_turns: usize,
    budget: Budget,
    tokenizer: Tokenizer,
) -> Result<Plan, TooLong> {
    let visible = in_view(items);
    let len = visible.len();
    let before = items.last().map_or(0, |item| item.sequence) + 1;
    let sizes = Sizes {
        whole: visible
            .iter()
            .map(|item| tokenizer.item(&item.data))
            .collect(),
        visible: &visible,
        tokenizer,
        before,
    };
    let starts = tail_starts(&visible);
    let group = request_group(&visible);
    let layout = |start: usize, chars: Option<u64>| {
        let mut chosen = group
            .iter()
            .filter(|&&index| index < start)
            .map(|&index| visible[index].sequence)
            .collect::<HashSet<_>>();
        chosen.extend(visible[start..].iter().map(|item| item.sequence));
        let (tail_start, kept) = arrange(items, &chosen);
        Plan {
            tail_start,
            kept,
            output_chars: chars,
        }
    };
    let Some(&opening) = group.first() else {
        // Nothing the person wrote is in view: whole turns, as they fit.
        let mut start = None;
        let mut users = 0;
        for index in (0..len).rev() {
            if !budget.fits(sizes.kept(index, &[], None), budget.soft) {
                break;
            }
            if matches!(visible[index].data, ItemData::UserMessage { .. }) && starts[index] {
                start = Some(index);
                users += 1;
                if users >= tail_turns {
                    break;
                }
            }
        }
        return Ok(start.map_or_else(Plan::default, |start| layout(start, None)));
    };
    let request = group.iter().map(|&index| sizes.whole[index]).sum::<u64>();
    if !budget.fits(request, budget.hard) {
        return Err(TooLong {
            request,
            room: budget.hard,
        });
    }
    // Whole recent turns, the request's among them.
    let mut whole_turns = None;
    let mut users = 0;
    for start in (0..=opening).rev() {
        if !budget.fits(sizes.kept(start, &group, None), budget.soft) {
            break;
        }
        let item = visible[start];
        if starts[start] && written(item) && (start == opening || !queued(item)) {
            whole_turns = Some(start);
            users += 1;
            if users >= tail_turns.max(1) {
                break;
            }
        }
    }
    if let Some(start) = whole_turns {
        return Ok(layout(start, None));
    }
    // Where the round of the newest result since the request starts: the
    // kept part holds at least that call and its result.
    let newest = visible
        .iter()
        .rposition(|item| matches!(item.data, ItemData::ToolResult { .. }))
        .filter(|&result| result > opening)
        .and_then(|result| (0..=result).rev().find(|&start| starts[start]))
        .map(|mut start| {
            // With what the model wrote before those calls.
            while start > opening + 1
                && starts[start - 1]
                && matches!(
                    visible[start - 1].data,
                    ItemData::AssistantMessage { .. } | ItemData::Reasoning { .. }
                )
            {
                start -= 1;
            }
            start
        });
    // The request, then the newest rounds of its turn that fit whole.
    let mut rounds = None;
    for start in (opening + 1..len).rev() {
        if !budget.fits(sizes.kept(start, &group, None), budget.soft) {
            break;
        }
        if starts[start] && newest.is_none_or(|newest| start <= newest) {
            rounds = Some(start);
        }
    }
    if let Some(start) = rounds {
        return Ok(layout(start, None));
    }
    // The request and its newest round, tool outputs cut as far as needed.
    if let Some(start) = newest {
        let longest = sizes.longest_output(start);
        for target in [budget.soft, budget.hard] {
            if budget.fits(sizes.kept(start, &group, None), target) {
                return Ok(layout(start, None));
            }
            if !budget.fits(
                sizes.kept(start, &group, Some(MIN_KEPT_OUTPUT_CHARS)),
                target,
            ) {
                continue;
            }
            // The most characters each output can keep.
            let (mut low, mut high) = (MIN_KEPT_OUTPUT_CHARS, longest.max(MIN_KEPT_OUTPUT_CHARS));
            while low < high {
                let middle = low + (high - low).div_ceil(2);
                if budget.fits(sizes.kept(start, &group, Some(middle)), target) {
                    low = middle;
                } else {
                    high = middle - 1;
                }
            }
            return Ok(layout(start, Some(low)));
        }
    }
    // The request alone.
    Ok(layout(len, None))
}

/// The kept part `chosen` (sequences of items in view) as a compaction
/// records it: the tail, from the earliest sequence after which every item
/// is chosen, and the chosen items before it. Items a previous compaction
/// hid stay hidden.
fn arrange(items: &[Item], chosen: &HashSet<u64>) -> (Option<u64>, Vec<u64>) {
    let raw = items
        .iter()
        .filter(|item| !matches!(item.data, ItemData::Compaction { .. }))
        .collect::<Vec<_>>();
    let mut start = raw.len();
    while start > 0 && chosen.contains(&raw[start - 1].sequence) {
        start -= 1;
    }
    let tail_start = raw.get(start).map(|item| item.sequence);
    let mut kept = chosen
        .iter()
        .copied()
        .filter(|sequence| tail_start.is_none_or(|tail| *sequence < tail))
        .collect::<Vec<_>>();
    kept.sort_unstable();
    (tail_start, kept)
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use haarness_protocol::tokens::{self, Tokenizer};
    use haarness_protocol::{
        CompactRequest, Item, ItemData, ItemId, PromptUsage, ThreadId, TurnId,
    };
    use serde_json::{Value, json};

    use crate::ProviderMessage;

    const SMALL_WINDOW: u64 = 32_768;

    struct Chat {
        items: Vec<Item>,
        thread: ThreadId,
    }

    impl Chat {
        fn new() -> Self {
            Self {
                items: Vec::new(),
                thread: ThreadId::new(),
            }
        }

        fn push(&mut self, turn: Option<TurnId>, data: ItemData) -> ItemId {
            let id = ItemId::new();
            self.items.push(Item {
                id,
                thread_id: self.thread,
                turn_id: turn,
                sequence: self.items.len() as u64 + 1,
                data,
                created_at: Utc::now(),
            });
            id
        }

        fn user(&mut self, turn: TurnId, text: &str) -> ItemId {
            self.message(turn, text, Vec::new(), false, false)
        }

        fn message(
            &mut self,
            turn: TurnId,
            text: &str,
            attachments: Vec<String>,
            queued: bool,
            synthetic: bool,
        ) -> ItemId {
            self.push(
                Some(turn),
                ItemData::UserMessage {
                    text: text.into(),
                    attachments,
                    queued,
                    synthetic,
                    command_line: None,
                },
            )
        }

        fn assistant(&mut self, turn: TurnId, text: &str) {
            self.push(Some(turn), ItemData::AssistantMessage { text: text.into() });
        }

        fn call(&mut self, turn: TurnId, id: &str, name: &str, arguments: Value) {
            self.push(
                Some(turn),
                ItemData::ToolCall {
                    call_id: id.into(),
                    name: name.into(),
                    arguments,
                    provider_metadata: Value::Null,
                },
            );
        }

        fn result(&mut self, turn: TurnId, id: &str, name: &str, output: &str, metadata: Value) {
            self.push(
                Some(turn),
                ItemData::ToolResult {
                    call_id: id.into(),
                    name: name.into(),
                    output: output.into(),
                    is_error: false,
                    metadata,
                },
            );
        }

        /// A provider report for a request whose instructions and tools
        /// took 13K tokens, as Haarness's do with a small local model.
        fn usage(&mut self, turn: TurnId) {
            self.push(
                Some(turn),
                ItemData::Usage {
                    prompt: Some(PromptUsage {
                        system_tokens: 10_000,
                        tool_tokens: 3_000,
                        history_tokens: 400,
                        media_attachments: 0,
                        media_tokens: 0,
                    }),
                    input_tokens: 13_400,
                    output_tokens: 120,
                    cache_read_tokens: 0,
                    cache_write_tokens: 0,
                    cost_usd: None,
                    estimated: false,
                    first_token_ms: None,
                    stream_ms: None,
                    stream_tokens: None,
                },
            );
        }

        /// An earlier finished exchange.
        fn exchange(&mut self, question: &str, answer: &str) -> TurnId {
            let turn = TurnId::new();
            self.user(turn, question);
            self.assistant(turn, answer);
            self.usage(turn);
            turn
        }

        /// The chat after an automatic compaction for a model with `window`
        /// tokens, as the next request sees it, or why it could not compact.
        fn compact(&self, window: u64) -> Result<Compacted, String> {
            let settings = crate::CompactionSettings::default();
            let limit = super::super::compaction_input_limit(&settings, window, None);
            let request = super::super::complete_compaction_request(
                &settings,
                &self.items,
                CompactRequest {
                    summary: Some("The person asked about a library; work is under way.".into()),
                    files_touched: Vec::new(),
                    outstanding: Vec::new(),
                    todo_checkpoint: None,
                    tail_start_sequence: None,
                    kept_sequences: Vec::new(),
                    tool_output_chars: None,
                },
                Some(limit),
                Tokenizer::Generic,
                1.0,
            )
            .map_err(|error| super::super::describe_failure(&error, None, true))?;
            let mut items = self.items.clone();
            items.push(Item {
                id: ItemId::new(),
                thread_id: self.thread,
                turn_id: None,
                sequence: items.len() as u64 + 1,
                data: ItemData::Compaction {
                    summary: request.summary.unwrap_or_default(),
                    files_touched: request.files_touched,
                    outstanding: request.outstanding,
                    todo_checkpoint: request.todo_checkpoint,
                    tail_start_sequence: request.tail_start_sequence,
                    kept_sequences: request.kept_sequences,
                    tool_output_chars: request.tool_output_chars,
                },
                created_at: Utc::now(),
            });
            Ok(Compacted { items, limit })
        }
    }

    struct Compacted {
        items: Vec<Item>,
        limit: u64,
    }

    impl Compacted {
        fn messages(&self) -> Vec<ProviderMessage> {
            let messages = super::super::provider_messages(self.items.clone());
            assert_paired(&messages);
            messages
        }

        /// The next request's estimate stays under the size that compacts,
        /// so the turn goes on rather than compacting again.
        fn assert_fits(&self) {
            let estimate = tokens::context(&self.items, Tokenizer::Generic).tokens;
            assert!(
                estimate <= self.limit,
                "the compacted chat still takes {estimate} tokens of {}",
                self.limit
            );
        }
    }

    /// Every result follows its call, and the calls of one response are
    /// answered before anything else is said.
    fn assert_paired(messages: &[ProviderMessage]) {
        let mut open = Vec::<String>::new();
        for message in messages {
            match message {
                ProviderMessage::ToolCall { call_id, .. } => open.push(call_id.clone()),
                ProviderMessage::ToolResult { call_id, .. } => {
                    let at = open.iter().position(|id| id == call_id);
                    assert!(at.is_some(), "a result without its call: {call_id}");
                    open.remove(at.unwrap());
                }
                _ => assert!(open.is_empty(), "calls left unanswered: {open:?}"),
            }
        }
        assert!(open.is_empty(), "calls left unanswered: {open:?}");
    }

    fn user_texts(messages: &[ProviderMessage]) -> Vec<&str> {
        messages
            .iter()
            .filter_map(|message| match message {
                ProviderMessage::Text { role, content }
                | ProviderMessage::Media { role, content, .. }
                    if role == "user" =>
                {
                    Some(content.as_str())
                }
                _ => None,
            })
            .collect()
    }

    fn result_of<'a>(messages: &'a [ProviderMessage], id: &str) -> &'a str {
        messages
            .iter()
            .find_map(|message| match message {
                ProviderMessage::ToolResult {
                    call_id, output, ..
                } if call_id == id => Some(output.as_str()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no result for {id} in {messages:#?}"))
    }

    /// A docs page of about `chars` characters, prose and markdown as real
    /// docs mix them, that starts with `PAGE-START` and ends with `PAGE-END`.
    fn page(chars: usize) -> String {
        use std::fmt::Write as _;
        let mut text = String::from("PAGE-START\n# Retry settings\n\n");
        let mut line = 0;
        while text.len() + 10 < chars {
            line += 1;
            let _ = write!(
                text,
                "The client retries a failed request with a backoff that grows each time.\n\
                 - [`retry.backoff_{line}`](/docs/api#retry-{line}): `base * 2^attempt`, capped at `max_delay` (`30s`).\n"
            );
        }
        text.truncate(chars - 9);
        text.push_str("\nPAGE-END");
        text
    }

    const QUESTION: &str =
        "Fetch https://example.com/docs/retries and tell me how the backoff grows.";

    /// A chat whose newest request fetched a page larger than the part of a
    /// 32K window that compaction keeps.
    fn fetched_page_chat() -> Chat {
        let mut chat = Chat::new();
        chat.exchange(
            "Which HTTP client does this project use?",
            "It uses reqwest.",
        );
        let turn = TurnId::new();
        chat.user(turn, QUESTION);
        chat.assistant(turn, "I'll fetch the page.");
        chat.call(
            turn,
            "fetch-1",
            "webfetch",
            json!({"url": "https://example.com/docs/retries"}),
        );
        chat.result(
            turn,
            "fetch-1",
            "webfetch",
            &page(48_877),
            json!({"outputPath": "/data/tool-output/fetched-1.md"}),
        );
        chat
    }

    #[test]
    fn the_request_stays_when_the_newest_output_is_larger_than_the_kept_part() {
        let compacted = fetched_page_chat().compact(SMALL_WINDOW).unwrap();
        let messages = compacted.messages();
        assert!(
            user_texts(&messages).contains(&QUESTION),
            "the request is gone: {:#?}",
            user_texts(&messages)
        );
        let output = result_of(&messages, "fetch-1");
        assert!(output.starts_with("PAGE-START"), "{}", &output[..200]);
        assert!(
            output.ends_with("PAGE-END"),
            "{}",
            &output[output.len() - 200..]
        );
        assert!(output.len() < 48_877);
        assert!(output.contains("characters"), "the cut is not explained");
        assert!(output.contains("/data/tool-output/fetched-1.md"));
        compacted.assert_fits();
    }

    #[test]
    fn several_oversized_outputs_are_all_cut_and_kept_with_their_calls() {
        let mut chat = Chat::new();
        chat.exchange("Hi", "Hello, what can I do?");
        let turn = TurnId::new();
        chat.user(turn, QUESTION);
        for id in ["a", "b", "c"] {
            chat.call(turn, id, "read", json!({"path": format!("docs/{id}.md")}));
        }
        for id in ["a", "b", "c"] {
            chat.result(turn, id, "read", &page(40_000), Value::Null);
        }
        let compacted = chat.compact(SMALL_WINDOW).unwrap();
        let messages = compacted.messages();
        assert!(user_texts(&messages).contains(&QUESTION));
        for id in ["a", "b", "c"] {
            let output = result_of(&messages, id);
            assert!(output.starts_with("PAGE-START") && output.ends_with("PAGE-END"));
            assert!(output.len() < 40_000);
            assert!(output.contains("offset"), "read says how to see the rest");
        }
        compacted.assert_fits();
    }

    #[test]
    fn a_request_too_long_for_the_window_fails_in_plain_words() {
        let mut chat = Chat::new();
        chat.exchange("Hi", "Hello");
        let turn = TurnId::new();
        let pasted = format!("Summarize this log:\n{}", page(300_000));
        chat.user(turn, &pasted);
        let error = chat
            .compact(SMALL_WINDOW)
            .err()
            .expect("compaction refuses");
        assert!(
            error.contains("too long for this model's context window"),
            "{error}"
        );
    }

    #[test]
    fn a_long_request_in_a_large_window_is_kept_whole() {
        let mut chat = Chat::new();
        chat.exchange("Hi", "Hello");
        let turn = TurnId::new();
        let pasted = format!("Summarize this log:\n{}", page(120_000));
        chat.user(turn, &pasted);
        chat.call(turn, "r", "read", json!({"path": "notes.md"}));
        chat.result(turn, "r", "read", &page(30_000), Value::Null);
        let compacted = chat.compact(200_000).unwrap();
        let messages = compacted.messages();
        assert!(user_texts(&messages).contains(&pasted.as_str()));
        assert!(result_of(&messages, "r").ends_with("PAGE-END"));
        compacted.assert_fits();
    }

    const PIXEL: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGP4z8AAAAMBAQDJ/pLvAAAAAElFTkSuQmCC";

    #[test]
    fn the_request_keeps_its_images() {
        // Too large to keep whole: the request is kept beside the cut page.
        let mut chat = Chat::new();
        chat.exchange("Hi", "Hello");
        let turn = TurnId::new();
        chat.message(
            turn,
            "What does this screen show? Check the docs page too.",
            vec![PIXEL.into()],
            false,
            false,
        );
        chat.call(
            turn,
            "fetch-1",
            "webfetch",
            json!({"url": "https://example.com/docs"}),
        );
        chat.result(turn, "fetch-1", "webfetch", &page(48_877), Value::Null);
        // Small enough to keep the whole turn.
        let mut small = Chat::new();
        small.exchange("Hi", "Hello");
        let second = TurnId::new();
        small.message(
            second,
            "What does this screen show?",
            vec![PIXEL.into()],
            false,
            false,
        );
        small.call(second, "r", "read", json!({"path": "notes.md"}));
        small.result(second, "r", "read", &page(30_000), Value::Null);
        for (chat, text) in [
            (chat, "What does this screen show? Check the docs page too."),
            (small, "What does this screen show?"),
        ] {
            let compacted = chat.compact(SMALL_WINDOW).unwrap();
            let messages = compacted.messages();
            let request = messages.iter().find(|message| {
                matches!(message, ProviderMessage::Text { content, .. } | ProviderMessage::Media { content, .. } if content.starts_with(text))
            });
            assert!(
                matches!(
                    request,
                    Some(ProviderMessage::Media { content, attachments, .. })
                        if content == text && attachments == &vec![PIXEL.to_owned()]
                ),
                "{request:#?}"
            );
        }
    }

    #[test]
    fn a_subagents_turn_keeps_its_task_and_the_steer_it_was_sent() {
        let mut chat = Chat::new();
        let turn = TurnId::new();
        let task =
            "Find how the retry backoff is configured in the docs and report the setting names.";
        chat.user(turn, task);
        chat.message(
            turn,
            "<system-reminder>Today is 2026-11-08.</system-reminder>",
            Vec::new(),
            false,
            true,
        );
        chat.call(turn, "read-1", "read", json!({"path": "docs/retries.md"}));
        // The parent sent more while the read ran.
        let steer = chat.message(
            turn,
            "Also note the default limit.",
            Vec::new(),
            true,
            false,
        );
        chat.result(turn, "read-1", "read", &page(30_000), Value::Null);
        chat.push(Some(turn), ItemData::QueueConsumed { item_id: steer });
        chat.usage(turn);
        chat.call(
            turn,
            "fetch-1",
            "webfetch",
            json!({"url": "https://example.com/docs/retries"}),
        );
        chat.result(turn, "fetch-1", "webfetch", &page(48_877), Value::Null);
        let compacted = chat.compact(SMALL_WINDOW).unwrap();
        let messages = compacted.messages();
        let texts = user_texts(&messages);
        assert!(texts.contains(&task), "{texts:#?}");
        assert_eq!(
            texts
                .iter()
                .filter(|text| text.contains("Also note the default limit."))
                .count(),
            1,
            "{texts:#?}"
        );
        assert!(result_of(&messages, "fetch-1").ends_with("PAGE-END"));
        compacted.assert_fits();
    }

    #[test]
    fn a_chat_compacted_before_keeps_the_new_request_and_not_what_the_first_summary_hid() {
        let mut chat = Chat::new();
        chat.exchange("first question, long ago", "first answer");
        let turn = TurnId::new();
        chat.user(turn, "Read the changelog.");
        chat.call(turn, "r1", "read", json!({"path": "CHANGELOG.md"}));
        chat.result(
            turn,
            "r1",
            "read",
            &format!("HIDDEN-CHANGELOG\n{}", page(40_000)),
            Value::Null,
        );
        chat.call(turn, "r2", "read", json!({"path": "NEWS.md"}));
        chat.result(
            turn,
            "r2",
            "read",
            "Version 2.0 renamed the retry option.",
            Value::Null,
        );
        chat.usage(turn);
        // The first compaction keeps the request and the newest read, and
        // hides the first exchange and the long read.
        let first = chat.compact(SMALL_WINDOW).unwrap();
        let texts = |compacted: &Compacted| {
            compacted
                .messages()
                .iter()
                .map(|message| format!("{message:?}"))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let seen = texts(&first);
        assert!(seen.contains("Read the changelog."), "{seen}");
        assert!(!seen.contains("HIDDEN-CHANGELOG") && !seen.contains("first question"));
        // A second one that keeps whole turns back to that request brings
        // back nothing the first one hid.
        let mut chat = Chat {
            items: first.items,
            thread: chat.thread,
        };
        let turn = TurnId::new();
        chat.user(turn, "Thanks. What else changed in 2.0?");
        chat.assistant(turn, "Nothing else is listed.");
        let second = chat.compact(SMALL_WINDOW).unwrap();
        let seen = texts(&second);
        assert!(seen.contains("Read the changelog.") && seen.contains("What else changed in 2.0?"));
        assert!(seen.contains("Version 2.0 renamed the retry option."));
        assert!(
            !seen.contains("HIDDEN-CHANGELOG") && !seen.contains("first question"),
            "{seen}"
        );
        second.assert_fits();
        // A third, after a long fetch, keeps the new request and cuts the page.
        let mut chat = Chat {
            items: second.items,
            thread: chat.thread,
        };
        let turn = TurnId::new();
        chat.user(turn, QUESTION);
        chat.call(
            turn,
            "fetch-1",
            "webfetch",
            json!({"url": "https://example.com/docs/retries"}),
        );
        chat.result(turn, "fetch-1", "webfetch", &page(48_877), Value::Null);
        let third = chat.compact(SMALL_WINDOW).unwrap();
        let messages = third.messages();
        let user = user_texts(&messages);
        assert!(user.contains(&QUESTION), "{user:#?}");
        assert_eq!(
            user.iter()
                .filter(|text| text.contains("<compaction-summary>"))
                .count(),
            1
        );
        assert!(!texts(&third).contains("HIDDEN-CHANGELOG"));
        assert!(result_of(&messages, "fetch-1").ends_with("PAGE-END"));
        third.assert_fits();
    }

    #[test]
    fn the_summary_request_cuts_long_outputs_to_fit_the_window() {
        let messages = || {
            vec![
                ProviderMessage::Text {
                    role: "user".into(),
                    content: QUESTION.into(),
                },
                ProviderMessage::ToolCall {
                    call_id: "fetch-1".into(),
                    name: "webfetch".into(),
                    arguments: json!({"url": "https://example.com/docs/retries"}),
                    provider_metadata: Value::Null,
                },
                ProviderMessage::ToolResult {
                    call_id: "fetch-1".into(),
                    name: "webfetch".into(),
                    output: page(200_000),
                    is_error: false,
                    attachments: Vec::new(),
                },
            ]
        };
        let size = |messages: &[ProviderMessage]| {
            messages
                .iter()
                .map(|message| match message {
                    ProviderMessage::Text { content, .. } => Tokenizer::Generic.text(content),
                    ProviderMessage::ToolResult { output, .. } => Tokenizer::Generic.text(output),
                    _ => 0,
                })
                .sum::<u64>()
        };
        let mut fitted = messages();
        super::super::fit_summary_input(&mut fitted, 20_000, Tokenizer::Generic);
        assert!(size(&fitted) <= 20_000, "{}", size(&fitted));
        assert!(user_texts(&fitted).contains(&QUESTION));
        let output = result_of(&fitted, "fetch-1");
        assert!(output.starts_with("PAGE-START") && output.ends_with("PAGE-END"));
        assert!(output.contains("characters are left out"));
        // A request that fits goes as it is.
        let mut whole = messages();
        super::super::fit_summary_input(&mut whole, 200_000, Tokenizer::Generic);
        assert_eq!(result_of(&whole, "fetch-1").len(), 200_000);
    }

    #[test]
    fn a_long_turn_keeps_its_request_and_newest_calls() {
        let mut chat = Chat::new();
        chat.exchange("Hi", "Hello");
        let turn = TurnId::new();
        chat.user(turn, "Rename the retry option everywhere in the docs.");
        for round in 0..80 {
            let id = format!("edit-{round}");
            chat.call(
                turn,
                &id,
                "read",
                json!({"path": format!("docs/page-{round}.md")}),
            );
            chat.result(turn, &id, "read", &page(3_000), Value::Null);
        }
        let compacted = chat.compact(SMALL_WINDOW).unwrap();
        let messages = compacted.messages();
        assert!(user_texts(&messages).contains(&"Rename the retry option everywhere in the docs."));
        assert!(result_of(&messages, "edit-79").ends_with("PAGE-END"));
        compacted.assert_fits();
    }
}

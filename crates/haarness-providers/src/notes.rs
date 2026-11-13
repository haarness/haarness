//! Notes Claude writes for the person before a tool call.
//!
//! Claude 5.5 (and Fable and Mythos 5) put the short line meant for the person
//! just before a call ("I'll run the tests now") in a thinking block of its
//! own. Under summarized thinking the block looks like any other; only its
//! signature tells. The adapters read each block's signature when the block
//! ends and send a note on as [`ProviderEvent::Note`]; the block itself goes
//! back to the model with the call, unchanged, like every thinking block.
//!
//! The signature comes last, so a block's text waits (see [`NoteHold`]) for
//! a short while: a note then goes out whole as reply text and never shows
//! inside "Thinking" first.

use std::time::Duration;

use futures::{Stream, StreamExt};
use haarness_core::ProviderEvent;
use tokio::time::Instant;

/// How many characters of a thinking block wait for its end before they go
/// out as reasoning. Notes are a sentence or two, well under this.
const HOLD_CHARS: usize = 600;
/// How long a thinking block's first text waits for the block's end.
const HOLD_TIME: Duration = Duration::from_secs(3);

/// The kind a note's signature names.
const NOTE_KIND: &[u8] = b"narration";

/// Whether a thinking block whose signature is `signature` is a note.
///
/// A signature is base64 over a protobuf message. Its field 2 holds a message
/// whose field 1 is a header in the clear, and the header's field 8 names
/// the block's kind: `narration` for a note, `thinking` for reasoning. The
/// rest is sealed. A signature laid out any other way (cut short, empty, a
/// layout Anthropic moves to later) is taken for reasoning: such a note is
/// drawn the way every note was before this reader.
pub(crate) fn is_note(signature: &str) -> bool {
    signature_kind(signature).as_deref() == Some(NOTE_KIND)
}

/// The kind the header of `signature` names, when it reads that way.
fn signature_kind(signature: &str) -> Option<Vec<u8>> {
    use base64::Engine as _;
    let signature = signature.trim();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(signature)
        .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(signature))
        .ok()?;
    [2, 1, 8]
        .into_iter()
        .try_fold(bytes.as_slice(), |message, number| field(message, number))
        .map(<[u8]>::to_vec)
}

/// A protobuf varint at the start of `bytes`, and the bytes after it.
fn varint(bytes: &[u8]) -> Option<(u64, &[u8])> {
    let mut value = 0_u64;
    for (index, byte) in bytes.iter().enumerate().take(10) {
        value |= u64::from(byte & 0x7f) << (7 * index);
        if byte & 0x80 == 0 {
            return Some((value, &bytes[index + 1..]));
        }
    }
    None
}

/// The body of the first length-delimited field `number` of the protobuf
/// message `bytes`, or `None` when it has none or stops making sense first
/// (a length past its end, a group, a field numbered 0).
fn field(mut bytes: &[u8], number: u64) -> Option<&[u8]> {
    while !bytes.is_empty() {
        let (key, rest) = varint(bytes)?;
        if key >> 3 == 0 {
            return None;
        }
        bytes = match key & 7 {
            0 => varint(rest)?.1,
            1 => rest.get(8..)?,
            2 => {
                let (length, rest) = varint(rest)?;
                let length = usize::try_from(length).ok()?;
                let body = rest.get(..length)?;
                if key >> 3 == number {
                    return Some(body);
                }
                &rest[length..]
            }
            5 => rest.get(4..)?,
            _ => return None,
        };
    }
    None
}

/// Whether `model` writes its notes as thinking blocks, so its thinking text
/// waits to be told apart: Claude 5.5 onwards, Fable and Mythos 5. Every
/// model's signatures are read; for the others a note, should one come, goes
/// out as reasoning first and is then moved to the reply.
pub(crate) fn writes_notes(model: &str) -> bool {
    crate::anthropic::claude_release(model).is_some_and(|release| {
        release.version >= (5, 5)
            || (matches!(release.family.as_str(), "fable" | "mythos") && release.version >= (5, 0))
    })
}

/// The open thinking block's text on its way out. While the model writes
/// notes, the text waits until the block ends, runs past [`HOLD_CHARS`] or
/// has waited [`HOLD_TIME`]; at its end the signature decides whether it was
/// a note.
#[derive(Debug, Default)]
pub(crate) struct NoteHold {
    /// How long a block's text waits; nothing waits when zero.
    wait: Duration,
    /// The open block's text as written.
    text: String,
    /// What waits to go out as reasoning: the break before the block, then
    /// its text.
    held: String,
    /// When the open block began to wait.
    since: Option<Instant>,
    /// Some of the open block already went out as reasoning.
    released: bool,
}

impl NoteHold {
    pub(crate) fn new(waits: bool) -> Self {
        Self {
            wait: if waits { HOLD_TIME } else { Duration::ZERO },
            ..Self::default()
        }
    }

    /// More text of the open block, after `lead` (the break that separates
    /// it from the block before, on its first text). What goes out now: the
    /// break and the text as reasoning when nothing waits, `ContentStarted`
    /// when the block begins to wait, everything that waited once the block
    /// runs long.
    pub(crate) fn push(&mut self, lead: &str, delta: &str) -> Vec<ProviderEvent> {
        self.text.push_str(delta);
        if self.wait.is_zero() || self.released {
            self.released = true;
            return [lead, delta]
                .into_iter()
                .filter(|text| !text.is_empty())
                .map(|text| ProviderEvent::ReasoningDelta(text.to_owned()))
                .collect();
        }
        self.held.push_str(lead);
        self.held.push_str(delta);
        if self.text.chars().count() > HOLD_CHARS {
            return self.release().into_iter().collect();
        }
        match self.since {
            Some(_) => Vec::new(),
            None => {
                self.since = Some(Instant::now());
                vec![ProviderEvent::ContentStarted]
            }
        }
    }

    /// When the waiting text is due to go out as reasoning.
    pub(crate) fn due(&self) -> Option<Instant> {
        self.since
            .filter(|_| !self.released && !self.held.is_empty())
            .map(|since| since + self.wait)
    }

    /// The waiting text, out as reasoning: the block waited long enough.
    pub(crate) fn release(&mut self) -> Option<ProviderEvent> {
        if self.held.is_empty() {
            return None;
        }
        self.released = true;
        Some(ProviderEvent::ReasoningDelta(std::mem::take(
            &mut self.held,
        )))
    }

    /// The open block's text as written so far.
    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    /// The open block ended before any signature came for it, closed by
    /// words, a call or the end of the answer: the stream carries no
    /// signatures (a gateway that drops them), so from now on nothing of it
    /// waits for a note that cannot be told. What waited goes out as
    /// reasoning.
    pub(crate) fn end_unsigned(&mut self) -> Option<ProviderEvent> {
        if !self.text.is_empty() {
            self.wait = Duration::ZERO;
        }
        self.end(None)
    }

    /// The open block ended with `signature` (none when it came without
    /// one): it is a note, or what still waited of it goes out as reasoning.
    pub(crate) fn end(&mut self, signature: Option<&str>) -> Option<ProviderEvent> {
        let text = std::mem::take(&mut self.text);
        let held = std::mem::take(&mut self.held);
        let released = std::mem::take(&mut self.released);
        self.since = None;
        if signature.is_some_and(is_note) && !text.trim().is_empty() {
            return Some(ProviderEvent::Note {
                text,
                streamed_as_reasoning: released,
            });
        }
        (!held.is_empty()).then_some(ProviderEvent::ReasoningDelta(held))
    }
}

/// The next item of `stream`, or `None` when `due` comes first.
pub(crate) async fn next_before<S>(stream: &mut S, due: Option<Instant>) -> Option<Option<S::Item>>
where
    S: Stream + Unpin,
{
    match due {
        Some(due) => tokio::select! {
            item = stream.next() => Some(item),
            () = tokio::time::sleep_until(due) => None,
        },
        None => Some(stream.next().await),
    }
}

/// Waits until `due`, or for ever without one.
pub(crate) async fn until(due: Option<Instant>) {
    match due {
        Some(due) => tokio::time::sleep_until(due).await,
        None => std::future::pending().await,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A protobuf length-delimited field.
    fn delimited(number: u8, body: &[u8]) -> Vec<u8> {
        let mut bytes = vec![(number << 3) | 2, u8::try_from(body.len()).unwrap()];
        bytes.extend_from_slice(body);
        bytes
    }

    /// A signature laid out as Claude's are, whose header names `kind`, with
    /// `before` (other fields) ahead of the header and a sealed part after it.
    pub(crate) fn signature_with(kind: &str, before: &[u8]) -> String {
        use base64::Engine as _;
        let mut header = vec![0x08, 0x13, 0x10, 0x02, 0x18, 0x02];
        header.extend(delimited(8, kind.as_bytes()));
        let mut inner = before.to_vec();
        inner.extend(delimited(1, &header));
        inner.extend(delimited(3, &[0x5a; 12]));
        let mut outer = vec![0x08, 0x04];
        outer.extend(delimited(2, &inner));
        base64::engine::general_purpose::STANDARD.encode(outer)
    }

    pub(crate) fn note_signature() -> String {
        signature_with("narration", &[])
    }

    pub(crate) fn thinking_signature() -> String {
        signature_with("thinking", &[])
    }

    #[test]
    fn the_kind_in_the_header_decides() {
        assert!(is_note(&note_signature()));
        assert!(!is_note(&thinking_signature()));
        assert!(!is_note(&signature_with("narrations", &[])));
        assert!(!is_note(&signature_with("narratio", &[])));
        // Varint, 64-bit and 32-bit fields ahead of the header are passed over.
        let before = [
            0x10, 0xac, 0x02, 0x19, 1, 2, 3, 4, 5, 6, 7, 8, 0x25, 1, 2, 3, 4,
        ];
        assert!(is_note(&signature_with("narration", &before)));
        assert!(!is_note(&signature_with("thinking", &before)));
        // Padding is optional.
        assert!(is_note(note_signature().trim_end_matches('=')));
    }

    /// The thinking block of a real Claude Sonnet 5.5 answer (recorded live,
    /// see `testdata/claude-sonnet-5-5-thinking-call.sse`) reads as reasoning
    /// through the same fields a note's would.
    #[test]
    fn a_real_thinking_signature_reads_as_thinking() {
        let signature = super::super::anthropic::tests::recorded_signature();
        assert_eq!(
            signature_kind(&signature).as_deref(),
            Some(&b"thinking"[..])
        );
        assert!(!is_note(&signature));
    }

    #[test]
    fn a_signature_it_cannot_read_is_reasoning() {
        use base64::Engine as _;
        let encode = |bytes: &[u8]| base64::engine::general_purpose::STANDARD.encode(bytes);
        let whole = note_signature();
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(&whole)
            .unwrap();
        for signature in [
            String::new(),
            "   ".into(),
            "narration".into(),
            "not base64 at all !!".into(),
            // Cut short: a length runs past the end.
            encode(&decoded[..decoded.len() / 2]),
            // A varint that never ends.
            encode(&[
                0x08, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
            ]),
            // A group, which no signature uses.
            encode(&[0x13, 0x01]),
            // A field numbered 0.
            encode(&[0x02, 0x00]),
            // The kind one level too shallow.
            encode(&delimited(2, &delimited(8, b"narration"))),
        ] {
            assert!(!is_note(&signature), "{signature:?}");
        }
    }

    #[test]
    fn notes_come_from_claude_5_5_and_fable_and_mythos_5() {
        for model in [
            "claude-opus-5-5",
            "claude-sonnet-5-5",
            "anthropic/claude-sonnet-5.5",
            "us.anthropic.claude-opus-5-5-v1:0",
            "claude-fable-5-1",
            "claude-mythos-5",
            "claude-opus-6",
            "claude-opus-5-5@20260901",
            "claude-sonnet-5-5[1m]",
        ] {
            assert!(writes_notes(model), "{model}");
        }
        for model in [
            "claude-opus-4-7",
            "claude-sonnet-5",
            "claude-sonnet-4-5-20250929",
            "claude-3-5-sonnet",
            "gpt-5",
            "deepseek-r1",
        ] {
            assert!(!writes_notes(model), "{model}");
        }
    }

    fn reasoning(events: impl IntoIterator<Item = ProviderEvent>) -> Option<String> {
        let texts: Vec<String> = events
            .into_iter()
            .filter_map(|event| match event {
                ProviderEvent::ReasoningDelta(text) => Some(text),
                _ => None,
            })
            .collect();
        (!texts.is_empty()).then(|| texts.concat())
    }

    #[test]
    fn a_short_note_goes_out_whole_and_never_as_reasoning() {
        let mut hold = NoteHold::new(true);
        assert!(matches!(
            hold.push("", "I'll run ").as_slice(),
            [ProviderEvent::ContentStarted]
        ));
        assert!(hold.push("", "the tests now.").is_empty());
        assert!(hold.due().is_some());
        match hold.end(Some(&note_signature())) {
            Some(ProviderEvent::Note {
                text,
                streamed_as_reasoning: false,
            }) => assert_eq!(text, "I'll run the tests now."),
            other => panic!("{other:?}"),
        }
        assert!(hold.due().is_none());
    }

    #[test]
    fn reasoning_waits_then_goes_out_with_its_break() {
        let mut hold = NoteHold::new(true);
        hold.push("\n\n", "Weighing the parser.");
        assert_eq!(
            reasoning(hold.end(Some(&thinking_signature()))).as_deref(),
            Some("\n\nWeighing the parser.")
        );
        // A block without a signature, or with one it cannot read, is
        // reasoning too.
        hold.push("", "More.");
        assert_eq!(reasoning(hold.end(None)).as_deref(), Some("More."));
        hold.push("", "Still more.");
        assert_eq!(
            reasoning(hold.end(Some("garbled"))).as_deref(),
            Some("Still more.")
        );
    }

    #[test]
    fn a_long_block_goes_out_as_reasoning_and_a_long_note_moves_after() {
        let mut hold = NoteHold::new(true);
        let long = "word ".repeat(HOLD_CHARS / 5 + 1);
        assert!(matches!(
            hold.push("", "Start. ").as_slice(),
            [ProviderEvent::ContentStarted]
        ));
        assert_eq!(
            reasoning(hold.push("", &long)),
            Some(format!("Start. {long}"))
        );
        // Once out, the rest streams as it comes.
        assert_eq!(reasoning(hold.push("", "End.")).as_deref(), Some("End."));
        match hold.end(Some(&note_signature())) {
            Some(ProviderEvent::Note {
                text,
                streamed_as_reasoning: true,
            }) => assert_eq!(text, format!("Start. {long}End.")),
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn waiting_text_goes_out_once_it_is_due() {
        assert_eq!(NoteHold::new(true).wait, HOLD_TIME);
        let mut hold = NoteHold {
            wait: Duration::from_millis(30),
            ..NoteHold::default()
        };
        let started = Instant::now();
        hold.push("", "Reading the file.");
        let due = hold.due().unwrap();
        let mut quiet = futures::stream::pending::<()>();
        assert!(next_before(&mut quiet, Some(due)).await.is_none());
        assert!(started.elapsed() >= Duration::from_millis(30));
        assert_eq!(
            reasoning(hold.release()).as_deref(),
            Some("Reading the file.")
        );
        assert!(hold.due().is_none());
        assert!(hold.end(Some(&thinking_signature())).is_none());
    }

    #[test]
    fn without_waiting_text_streams_and_a_note_still_moves() {
        let mut hold = NoteHold::new(false);
        assert_eq!(
            reasoning(hold.push("", "I'll check.")).as_deref(),
            Some("I'll check.")
        );
        assert!(hold.due().is_none());
        assert!(matches!(
            hold.end(Some(&note_signature())),
            Some(ProviderEvent::Note {
                streamed_as_reasoning: true,
                ..
            })
        ));
    }

    #[test]
    fn a_block_without_a_signature_stops_the_waiting() {
        let mut hold = NoteHold::new(true);
        // Nothing open: the stream may still send signatures.
        assert!(hold.end_unsigned().is_none());
        assert!(matches!(
            hold.push("", "Reading.").as_slice(),
            [ProviderEvent::ContentStarted]
        ));
        assert_eq!(reasoning(hold.end_unsigned()).as_deref(), Some("Reading."));
        // The rest of the stream streams as it comes.
        assert_eq!(reasoning(hold.push("", "More.")).as_deref(), Some("More."));
        assert!(hold.due().is_none());
    }

    #[test]
    fn a_blank_note_stays_reasoning() {
        let mut hold = NoteHold::new(true);
        hold.push("", "  ");
        assert!(matches!(
            hold.end(Some(&note_signature())),
            Some(ProviderEvent::ReasoningDelta(_))
        ));
    }
}

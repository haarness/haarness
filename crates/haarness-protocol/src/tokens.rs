//! Token accounting shared by compaction, the context meters and request usage.
//!
//! Providers report the exact prompt size after every request, so the context
//! the next request carries is anchored on that report: the last input plus
//! that round's output. Only what was appended since (tool results, new user
//! messages, media) is estimated, with weights fitted per tokenizer family to
//! the providers' own count endpoints and scaled by how far the previous
//! request's estimate was from its reported input. No tokenizer ships here:
//! Claude 4.7+ and Gemini 3 have no public one, and the estimated part is a
//! small share of any context close to its limit.

use std::io::Read as _;
use std::path::Path;

use crate::{Item, ItemData, PromptUsage};

/// Tokenizer family of a model. Models of one family split text alike, so one
/// set of weights serves all of them; see [`Tokenizer::for_model`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tokenizer {
    /// Claude Opus 4.7 and later, Sonnet 5, Fable: about a third more tokens
    /// than earlier Claude models for the same text.
    Claude,
    /// Claude 3 to 4.6 and Haiku 4.5.
    ClaudeLegacy,
    /// OpenAI GPT-4o and later, the o-series (o200k).
    OpenAi,
    /// Gemini and Gemma.
    Gemini,
    /// Any other model: the larger of the OpenAI and Gemini weights, corrected
    /// by calibration once the provider reports its first request.
    Generic,
}

/// Per character-class token weights, in [`CLASSES`] order. Fitted to exact
/// counts from each provider's count endpoint over code, docs, JSON, numbers
/// and 10 languages (2026-10-05); the 90th/10th percentile ratio of
/// actual/estimate is 1.22–1.26 per family, against 2–3.7 for chars/4.
const WEIGHTS: [[f64; 12]; 5] = [
    // letter digit space punct switch cjk hangul latin_ext cyrillic other typographic emoji
    [
        0.434, 0.542, 0.026, 0.779, 0.831, 1.008, 1.460, 2.063, 0.503, 0.919, 1.656, 2.577,
    ],
    [
        0.273, 0.551, 0.048, 0.705, 0.911, 0.998, 1.438, 2.255, 0.502, 0.912, 1.621, 2.580,
    ],
    [
        0.173, 0.556, 0.000, 0.752, 0.708, 0.765, 0.737, 1.957, 0.314, 0.437, 1.479, 1.783,
    ],
    [
        0.226, 1.157, 0.101, 0.695, 0.527, 0.593, 0.686, 1.095, 0.300, 0.395, 1.456, 0.997,
    ],
    [
        0.226, 1.157, 0.101, 0.752, 0.708, 0.765, 0.737, 1.957, 0.314, 0.437, 1.479, 1.783,
    ],
];

/// Classes counted by [`classes`]: ASCII letters, digits, whitespace and
/// punctuation; switches between lower case, upper case and digits inside an
/// ASCII word (identifiers, base64); then non-ASCII scripts and symbols.
const CLASSES: usize = 12;
const LETTER: usize = 0;
const DIGIT: usize = 1;
const SPACE: usize = 2;
const PUNCT: usize = 3;
const SWITCH: usize = 4;
const CJK: usize = 5;
const HANGUL: usize = 6;
const LATIN_EXT: usize = 7;
const CYRILLIC: usize = 8;
const OTHER_SCRIPT: usize = 9;
const TYPOGRAPHIC: usize = 10;
const EMOJI: usize = 11;

/// Role and turn framing every provider adds around a message.
const MESSAGE_OVERHEAD: u64 = 4;

/// Estimated history growth below which two reports are too close to calibrate.
const MIN_CALIBRATION_GROWTH: u64 = 1_000;

/// Calibration never moves an estimate further than this from the weights.
const CALIBRATION_RANGE: (f64, f64) = (0.75, 1.5);

/// Bytes read to find image dimensions; JPEG frames can follow a large EXIF
/// thumbnail.
const HEADER_BYTES: usize = 256 * 1024;

/// Attachments above this are never sent inline, so they are not read.
const MAX_MEDIA_BYTES: u64 = crate::MAX_INLINE_MEDIA_BYTES as u64;

impl Tokenizer {
    /// Family for a model id as any provider names it: `claude-sonnet-5-5`,
    /// `anthropic/claude-opus-4.6`, `us.anthropic.claude-haiku-4-5-v1:0`,
    /// `router/openai/gpt-5.5`, `gemini-3.8-flash`, `o3`.
    pub fn for_model(model: &str) -> Self {
        let model = model.to_ascii_lowercase();
        let name = model.rsplit('/').next().unwrap_or(&model);
        if let Some(index) = name.find("claude") {
            return claude_family(&name[index + "claude".len()..]);
        }
        if name.contains("gemini") || name.contains("gemma") {
            return Self::Gemini;
        }
        let o_series = name
            .strip_prefix('o')
            .and_then(|rest| rest.chars().next())
            .is_some_and(|c| c.is_ascii_digit());
        if o_series
            || name.starts_with("gpt-")
            || name.starts_with("chatgpt")
            || name.contains("codex")
            || model.starts_with("openai/")
            || model.contains("/openai/")
        {
            return Self::OpenAi;
        }
        Self::Generic
    }

    fn weights(self) -> &'static [f64; CLASSES] {
        &WEIGHTS[self as usize]
    }

    /// Estimated tokens for `text`, in one pass over its characters.
    pub fn text(self, text: &str) -> u64 {
        let counts = classes(text);
        #[allow(clippy::cast_precision_loss)]
        let total: f64 = counts
            .iter()
            .zip(self.weights())
            .map(|(&count, &weight)| count as f64 * weight)
            .sum();
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let tokens = total.ceil() as u64;
        tokens
    }

    /// Estimated tokens for one attachment (a local path, `file://` URL or
    /// data URL): images from their dimensions, PDFs from their page count,
    /// audio and video from their size. Never from the base64 length.
    pub fn media(self, source: &str) -> u64 {
        match MediaShape::of(source) {
            MediaShape::Image { width, height } => self.image(width, height),
            MediaShape::Pdf { pages } => {
                pages.max(1) * if self == Self::Gemini { 600 } else { 3_000 }
            }
            MediaShape::Audio { bytes } => (bytes / 500).max(100),
            MediaShape::Video { bytes } => (bytes / 800).max(300),
            MediaShape::Unknown => self.image(2_048, 2_048),
        }
    }

    /// Tokens for an image of `width`×`height` pixels, after the provider's own
    /// downscaling. Measured against each count endpoint (2026-10-05).
    pub fn image(self, width: u64, height: u64) -> u64 {
        let (width, height) = (width.max(1), height.max(1));
        let patches = |size: f64, max_edge: f64| {
            #[allow(clippy::cast_precision_loss)]
            let scale = (max_edge / width.max(height) as f64).min(1.0);
            #[allow(
                clippy::cast_precision_loss,
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss
            )]
            let count = |pixels: u64| ((pixels as f64 * scale / size).ceil() as u64).max(1);
            count(width) * count(height)
        };
        match self {
            // 28 px patches; long edge 2576 px, at most ~4784 tokens.
            Self::Claude | Self::Generic => patches(28.0, 2_576.0).min(4_784) + 3,
            Self::ClaudeLegacy => patches(28.0, 1_568.0).min(1_600) + 3,
            // 32 px patches at 1.2 tokens each, at most 10,000 patches.
            Self::OpenAi => (patches(32.0, f64::MAX).min(10_000) * 6).div_ceil(5),
            // Gemini 3 bills a fixed budget per image at its default resolution.
            Self::Gemini => 1_120,
        }
    }

    /// Tokens the provider receives for one transcript item; zero for items
    /// that never reach the provider (usage, todos, task links...).
    pub fn item(self, data: &ItemData) -> u64 {
        let text = |value: &str| self.text(value);
        let json = |value: &serde_json::Value| {
            if value.is_null() {
                0
            } else {
                self.text(&value.to_string())
            }
        };
        MESSAGE_OVERHEAD
            + match data {
                ItemData::UserMessage {
                    text: body,
                    attachments,
                    queued,
                    ..
                } => {
                    text(body)
                        + u64::from(*queued) * 30
                        + attachments
                            .iter()
                            .map(|source| self.media(source))
                            .sum::<u64>()
                }
                ItemData::AssistantMessage { text: body } => text(body),
                // Reasoning replayed through `provider_metadata` is opaque here;
                // the anchor's output tokens already include it.
                ItemData::ToolCall {
                    name, arguments, ..
                } => text(name) + json(arguments),
                ItemData::ToolResult {
                    name,
                    output,
                    metadata,
                    ..
                } => {
                    text(name)
                        + text(output)
                        + metadata
                            .get("attachments")
                            .and_then(serde_json::Value::as_array)
                            .map_or(0, |values| {
                                values
                                    .iter()
                                    .filter_map(serde_json::Value::as_str)
                                    .map(|source| self.media(source))
                                    .sum()
                            })
                }
                ItemData::Compaction {
                    summary,
                    files_touched,
                    outstanding,
                    todo_checkpoint,
                    ..
                } => {
                    60 + text(summary)
                        + files_touched
                            .iter()
                            .chain(outstanding)
                            .map(|line| text(line) + 1)
                            .sum::<u64>()
                        + todo_checkpoint
                            .as_ref()
                            .and_then(|todos| serde_json::to_string(todos).ok())
                            .map_or(0, |todos| text(&todos))
                }
                _ => return 0,
            }
    }
}

fn claude_family(rest: &str) -> Tokenizer {
    // `-opus-4-6`, `-sonnet-5.5`, `-3-5-sonnet-20241022`, `-haiku-4-5-20251001`
    let numbers: Vec<u64> = rest
        .split(|c: char| !c.is_ascii_digit())
        .filter(|part| !part.is_empty() && part.len() <= 2)
        .filter_map(|part| part.parse().ok())
        .take(2)
        .collect();
    match numbers.as_slice() {
        [major, ..] if *major >= 5 => Tokenizer::Claude,
        [4, minor, ..] if *minor >= 7 => Tokenizer::Claude,
        [major, ..] if *major <= 4 => Tokenizer::ClaudeLegacy,
        _ => Tokenizer::Claude,
    }
}

fn classes(text: &str) -> [u64; CLASSES] {
    let mut counts = [0_u64; CLASSES];
    // 0 outside an ASCII word, else 1 lower, 2 upper, 3 digit.
    let mut previous = 0_u8;
    for ch in text.chars() {
        let code = u32::from(ch);
        if ch.is_ascii() {
            let kind = match ch {
                'a'..='z' => 1,
                'A'..='Z' => 2,
                '0'..='9' => 3,
                _ => 0,
            };
            if kind != 0 && previous != 0 && kind != previous {
                counts[SWITCH] += 1;
            }
            previous = kind;
            counts[match kind {
                1 | 2 => LETTER,
                3 => DIGIT,
                _ if ch.is_ascii_whitespace() => SPACE,
                _ => PUNCT,
            }] += 1;
            continue;
        }
        previous = 0;
        counts[match code {
            0xAC00..=0xD7AF | 0x1100..=0x11FF | 0x3130..=0x318F => HANGUL,
            0x2E80..=0x9FFF | 0xF900..=0xFAFF | 0xFF00..=0xFFEF => CJK,
            0x80..=0x24F | 0x1E00..=0x1EFF => LATIN_EXT,
            0x400..=0x52F => CYRILLIC,
            _ if ch.is_alphabetic() => OTHER_SCRIPT,
            0x1F000.. | 0x2600..=0x27BF | 0xFE0F => EMOJI,
            _ => TYPOGRAPHIC,
        }] += 1;
    }
    counts
}

/// Items still sent to the provider after the newest compaction: its summary,
/// then the retained verbatim tail (whose rows have earlier sequences), then
/// everything after it.
pub fn visible_items(items: &[Item]) -> Vec<&Item> {
    let Some(index) = items
        .iter()
        .rposition(|item| matches!(item.data, ItemData::Compaction { .. }))
    else {
        return items.iter().collect();
    };
    let tail_start = match &items[index].data {
        ItemData::Compaction {
            tail_start_sequence,
            ..
        } => *tail_start_sequence,
        _ => None,
    };
    let mut visible = vec![&items[index]];
    if let Some(tail_start) = tail_start {
        visible.extend(items[..index].iter().filter(|item| {
            item.sequence >= tail_start && !matches!(item.data, ItemData::Compaction { .. })
        }));
    }
    visible.extend(&items[index + 1..]);
    visible
}

/// Size of the context the next provider request carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ContextEstimate {
    /// `reported + estimated`.
    pub tokens: u64,
    /// Exact part: the last provider report since the newest compaction, its
    /// input plus its output (an upper bound when the reasoning in that
    /// output is not sent back).
    pub reported: u64,
    /// Estimated part: everything appended since that report, or the whole
    /// retained history and the last known system prompt and tools when no
    /// report applies yet.
    pub estimated: u64,
}

/// The context of the next request for a thread's `items` (in sequence order,
/// undone history already removed).
pub fn context(items: &[Item], tokenizer: Tokenizer) -> ContextEstimate {
    let visible = visible_items(items);
    let compacted_at = items
        .iter()
        .rfind(|item| matches!(item.data, ItemData::Compaction { .. }))
        .map(|item| item.sequence);
    // A report kept in the retained tail measured the history before it was
    // summarized; only one made after the compaction anchors the estimate.
    let anchor = visible.iter().rposition(|item| {
        reported_usage(&item.data) && compacted_at.is_none_or(|sequence| item.sequence > sequence)
    });
    let (reported, appended, overhead) = match anchor {
        Some(index) => {
            let ItemData::Usage {
                input_tokens,
                output_tokens,
                ..
            } = &visible[index].data
            else {
                unreachable!("anchor is a usage item")
            };
            (input_tokens + output_tokens, &visible[index + 1..], 0)
        }
        None => {
            // System prompt and tools of the last request, scaled by how that
            // whole request compared with its report: providers render tool
            // schemas far more compactly than their JSON (OpenAI ~0.6).
            let overhead = items.iter().rev().find_map(|item| match &item.data {
                ItemData::Usage {
                    prompt: Some(prompt),
                    input_tokens,
                    ..
                } => {
                    let overhead = prompt.system_tokens + prompt.tool_tokens;
                    #[allow(clippy::cast_precision_loss)]
                    let ratio = prompt_total(prompt)
                        .map_or(1.0, |total| *input_tokens as f64 / total as f64);
                    #[allow(
                        clippy::cast_precision_loss,
                        clippy::cast_possible_truncation,
                        clippy::cast_sign_loss
                    )]
                    Some((overhead as f64 * ratio.clamp(0.25, 2.0)).ceil() as u64)
                }
                _ => None,
            });
            (0, visible.as_slice(), overhead.unwrap_or(0))
        }
    };
    let appended = appended
        .iter()
        .map(|item| tokenizer.item(&item.data))
        .sum::<u64>();
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    let estimated = overhead + (appended as f64 * calibration(items)).ceil() as u64;
    ContextEstimate {
        tokens: reported + estimated,
        reported,
        estimated,
    }
}

/// How the provider counts appended history against the estimate: the growth
/// of the reported input between the latest request and an earlier one of the
/// same history (no compaction between them, at least
/// [`MIN_CALIBRATION_GROWTH`] estimated tokens apart, nearly the same system
/// prompt and tools) over the growth of their estimates, within
/// [`CALIBRATION_RANGE`]; 1 without such a pair. A whole-request ratio would
/// carry the tool definitions' error into every estimate of new messages.
pub fn calibration(items: &[Item]) -> f64 {
    let fixed = |prompt: &PromptUsage| prompt.system_tokens + prompt.tool_tokens;
    let history = |prompt: &PromptUsage| prompt.history_tokens + prompt.media_tokens;
    let mut latest: Option<(&PromptUsage, u64)> = None;
    for item in items.iter().rev() {
        let (prompt, input) = match &item.data {
            ItemData::Compaction { .. } => break,
            // Older records counted attachments without estimating them.
            ItemData::Usage {
                prompt: Some(prompt),
                input_tokens,
                ..
            } if reported_usage(&item.data)
                && (prompt.media_attachments == 0 || prompt.media_tokens > 0) =>
            {
                (prompt, *input_tokens)
            }
            _ => continue,
        };
        let Some((late, late_input)) = latest else {
            latest = Some((prompt, input));
            continue;
        };
        let grown = history(late).saturating_sub(history(prompt));
        if grown < MIN_CALIBRATION_GROWTH {
            continue;
        }
        if fixed(late).abs_diff(fixed(prompt)) * 10 > grown || late_input <= input {
            break;
        }
        let estimated =
            (history(late) + fixed(late)).saturating_sub(history(prompt) + fixed(prompt));
        #[allow(clippy::cast_precision_loss)]
        let ratio = (late_input - input) as f64 / estimated.max(1) as f64;
        return ratio.clamp(CALIBRATION_RANGE.0, CALIBRATION_RANGE.1);
    }
    1.0
}

/// A usage record the provider reported: not a local estimate, not empty.
fn reported_usage(data: &ItemData) -> bool {
    matches!(
        data,
        ItemData::Usage {
            input_tokens,
            output_tokens,
            estimated: false,
            ..
        } if *input_tokens > 0 || *output_tokens > 0
    )
}

fn prompt_total(prompt: &PromptUsage) -> Option<u64> {
    Some(prompt.system_tokens + prompt.tool_tokens + prompt.history_tokens + prompt.media_tokens)
        .filter(|total| *total > 0)
}

enum MediaShape {
    Image { width: u64, height: u64 },
    Pdf { pages: u64 },
    Audio { bytes: u64 },
    Video { bytes: u64 },
    Unknown,
}

impl MediaShape {
    fn of(source: &str) -> Self {
        let (mime, head, size, whole) = if let Some(data) = source.strip_prefix("data:") {
            let Some((header, encoded)) = data.split_once(',') else {
                return Self::Unknown;
            };
            let mime = header
                .split(';')
                .next()
                .unwrap_or_default()
                .to_ascii_lowercase();
            let size = (encoded.len() as u64 / 4) * 3;
            let pdf = mime == "application/pdf";
            let prefix = if pdf {
                encoded
            } else {
                encoded.get(..HEADER_BYTES / 3 * 4).unwrap_or(encoded)
            };
            (mime, decode_base64(prefix), size, pdf)
        } else {
            let path = source.strip_prefix("file://").unwrap_or(source);
            let path = Path::new(path);
            let mime = mime_for_extension(path);
            let Ok(metadata) = std::fs::metadata(path) else {
                return Self::by_mime(&mime, 0);
            };
            let pdf = mime == "application/pdf";
            if !metadata.is_file() || metadata.len() > MAX_MEDIA_BYTES {
                return Self::by_mime(&mime, metadata.len());
            }
            let limit = if pdf {
                MAX_MEDIA_BYTES
            } else {
                HEADER_BYTES as u64
            };
            let mut head = Vec::new();
            if std::fs::File::open(path)
                .and_then(|file| file.take(limit).read_to_end(&mut head))
                .is_err()
            {
                return Self::by_mime(&mime, metadata.len());
            }
            (mime, head, metadata.len(), pdf)
        };
        if let Some((width, height)) = image_size(&head) {
            return Self::Image { width, height };
        }
        if head.starts_with(b"%PDF") {
            let pages = if whole { pdf_pages(&head) } else { 0 };
            return Self::Pdf {
                pages: if pages > 0 { pages } else { size / 100_000 + 1 },
            };
        }
        Self::by_mime(&mime, size)
    }

    fn by_mime(mime: &str, bytes: u64) -> Self {
        match mime.split('/').next() {
            Some("audio") => Self::Audio { bytes },
            Some("video") => Self::Video { bytes },
            _ if mime == "application/pdf" => Self::Pdf {
                pages: bytes / 100_000 + 1,
            },
            _ => Self::Unknown,
        }
    }
}

fn mime_for_extension(path: &Path) -> String {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match extension.as_str() {
        "pdf" => "application/pdf",
        "mp3" | "wav" | "m4a" | "aac" | "ogg" | "oga" | "flac" | "opus" | "aiff" | "aif" => {
            "audio/*"
        }
        "mp4" | "mov" | "webm" | "mkv" | "avi" | "mpeg" | "mpg" | "m4v" | "3gp" => "video/*",
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "heic" | "heif" => "image/*",
        _ => "",
    }
    .to_owned()
}

/// Width and height from a PNG, JPEG, GIF, WebP or BMP header.
fn image_size(bytes: &[u8]) -> Option<(u64, u64)> {
    let be16 = |at: usize| {
        Some(u64::from(u16::from_be_bytes(
            bytes.get(at..at + 2)?.try_into().ok()?,
        )))
    };
    let le16 = |at: usize| {
        Some(u64::from(u16::from_le_bytes(
            bytes.get(at..at + 2)?.try_into().ok()?,
        )))
    };
    let le24 = |at: usize| Some(u64::from(bytes.get(at)?.to_owned()) | le16(at + 1)? << 8);
    let be32 = |at: usize| {
        Some(u64::from(u32::from_be_bytes(
            bytes.get(at..at + 4)?.try_into().ok()?,
        )))
    };
    let le32 = |at: usize| {
        Some(u64::from(u32::from_le_bytes(
            bytes.get(at..at + 4)?.try_into().ok()?,
        )))
    };
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some((be32(16)?, be32(20)?));
    }
    if bytes.starts_with(b"GIF8") {
        return Some((le16(6)?, le16(8)?));
    }
    if bytes.starts_with(b"BM") {
        // Height is negative for top-down rows.
        let height = i32::from_le_bytes(bytes.get(22..26)?.try_into().ok()?).unsigned_abs();
        return Some((le32(18)?, height.into()));
    }
    if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        return match bytes.get(12..16)? {
            b"VP8 " => Some((le16(26)? & 0x3FFF, le16(28)? & 0x3FFF)),
            b"VP8L" => {
                let bits = le32(21)?;
                Some((1 + (bits & 0x3FFF), 1 + ((bits >> 14) & 0x3FFF)))
            }
            b"VP8X" => Some((1 + le24(24)?, 1 + le24(27)?)),
            _ => None,
        };
    }
    if bytes.starts_with(&[0xFF, 0xD8]) {
        let mut at = 2;
        while at + 9 < bytes.len() {
            if bytes[at] != 0xFF {
                return None;
            }
            let marker = bytes[at + 1];
            match marker {
                0xFF => at += 1,
                0x01 | 0xD0..=0xD8 => at += 2,
                0xC0..=0xCF if !matches!(marker, 0xC4 | 0xC8 | 0xCC) => {
                    return Some((be16(at + 7)?, be16(at + 5)?));
                }
                _ => at += 2 + usize::try_from(be16(at + 2)?).ok()?,
            }
        }
    }
    None
}

/// Page objects in a PDF whose object table is not compressed; zero otherwise.
fn pdf_pages(bytes: &[u8]) -> u64 {
    let mut pages = 0;
    for pattern in [&b"/Type /Page"[..], b"/Type/Page"] {
        pages += bytes
            .windows(pattern.len() + 1)
            .filter(|window| {
                window.starts_with(pattern) && !window[pattern.len()].is_ascii_alphabetic()
            })
            .count() as u64;
    }
    pages
}

/// Decodes standard base64, stopping at the first character outside it.
fn decode_base64(text: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let (mut buffer, mut bits) = (0_u32, 0_u32);
    for byte in text.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'\r' | b'\n' => continue,
            _ => break,
        };
        buffer = buffer << 6 | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            #[allow(clippy::cast_possible_truncation)]
            out.push((buffer >> bits) as u8);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use serde_json::json;
    use uuid::Uuid;

    fn item(sequence: u64, data: ItemData) -> Item {
        Item {
            id: crate::ItemId(Uuid::now_v7()),
            thread_id: crate::ThreadId(Uuid::nil()),
            turn_id: None,
            sequence,
            data,
            created_at: Utc::now(),
        }
    }

    fn usage(input_tokens: u64, output_tokens: u64, prompt: Option<PromptUsage>) -> ItemData {
        ItemData::Usage {
            prompt,
            input_tokens,
            output_tokens,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            cost_usd: None,
            estimated: false,
            first_token_ms: None,
            stream_ms: None,
            stream_tokens: None,
        }
    }

    fn user(text: &str) -> ItemData {
        ItemData::UserMessage {
            text: text.into(),
            attachments: Vec::new(),
            queued: false,
            synthetic: false,
            command_line: None,
        }
    }

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        bytes.extend(width.to_be_bytes());
        bytes.extend(height.to_be_bytes());
        bytes.extend([8, 2, 0, 0, 0]);
        bytes
    }

    fn base64(bytes: &[u8]) -> String {
        const ALPHABET: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        bytes
            .chunks(3)
            .flat_map(|chunk| {
                let word = chunk
                    .iter()
                    .enumerate()
                    .fold(0_u32, |word, (index, &byte)| {
                        word | u32::from(byte) << (16 - 8 * index)
                    });
                (0..=chunk.len())
                    .map(move |index| ALPHABET[(word >> (18 - 6 * index) & 63) as usize] as char)
            })
            .collect()
    }

    #[test]
    fn models_map_to_their_tokenizer() {
        for (model, family) in [
            ("claude-sonnet-5-5", Tokenizer::Claude),
            // Counted 2026-10-10: Haiku 5.5 matches Opus 5.5 token for token.
            ("claude-haiku-5-5", Tokenizer::Claude),
            ("anthropic/claude-haiku-5.5", Tokenizer::Claude),
            ("router/anthropic/claude-opus-4.7", Tokenizer::Claude),
            ("claude-fable-5-1", Tokenizer::Claude),
            ("claude-opus-4-6", Tokenizer::ClaudeLegacy),
            (
                "us.anthropic.claude-haiku-4-5-20251001-v1:0",
                Tokenizer::ClaudeLegacy,
            ),
            ("claude-3-5-sonnet-20241022", Tokenizer::ClaudeLegacy),
            ("anthropic/claude-sonnet-4.6", Tokenizer::ClaudeLegacy),
            ("gpt-5.5", Tokenizer::OpenAi),
            ("gpt-6.1-sol", Tokenizer::OpenAi),
            ("router/openai/gpt-5-mini", Tokenizer::OpenAi),
            ("o3", Tokenizer::OpenAi),
            ("o4-mini", Tokenizer::OpenAi),
            ("gpt-5.2-codex", Tokenizer::OpenAi),
            ("gemini-3.8-flash", Tokenizer::Gemini),
            ("google/gemini-4-argon", Tokenizer::Gemini),
            ("router/google/gemini-3.1-pro-preview", Tokenizer::Gemini),
            ("qwen3:0.6b", Tokenizer::Generic),
            ("deepseek/deepseek-v4", Tokenizer::Generic),
            ("ollama", Tokenizer::Generic),
        ] {
            assert_eq!(Tokenizer::for_model(model), family, "{model}");
        }
    }

    #[test]
    fn text_estimates_track_provider_counts() {
        // Exact counts from each provider's count endpoint, framing removed.
        let english = "The quick brown fox jumps over the lazy dog. ".repeat(20);
        let chinese =
            "人工智能代理在云端运行，它们的文件保存在持久磁盘上，每个请求都按照账本计费。"
                .repeat(12);
        for (tokenizer, text, actual) in [
            (Tokenizer::Claude, english.as_str(), 360),
            (Tokenizer::OpenAi, english.as_str(), 201),
            (Tokenizer::Gemini, english.as_str(), 201),
            (Tokenizer::Claude, chinese.as_str(), 480),
            (Tokenizer::OpenAi, chinese.as_str(), 336),
            (Tokenizer::Gemini, chinese.as_str(), 288),
        ] {
            let estimate = tokenizer.text(text);
            #[allow(clippy::cast_precision_loss)]
            let ratio = estimate as f64 / f64::from(actual);
            assert!(
                (0.7..=1.3).contains(&ratio),
                "{tokenizer:?} {estimate} vs {actual}"
            );
        }
        assert_eq!(Tokenizer::OpenAi.text(""), 0);
    }

    #[test]
    fn images_are_sized_from_their_header_not_their_base64() {
        let small = format!("data:image/png;base64,{}", base64(&png(512, 512)));
        let large = format!("data:image/png;base64,{}", base64(&png(4_000, 4_000)));
        // Measured: Sonnet 5.5 364/4761, Sonnet 4.6 364/1521, GPT-5.5 308/12001, Gemini 3.8 ~1090.
        assert_eq!(Tokenizer::Claude.media(&small), 364);
        assert_eq!(Tokenizer::Claude.media(&large), 4_787);
        assert_eq!(Tokenizer::ClaudeLegacy.media(&large), 1_603);
        assert_eq!(Tokenizer::OpenAi.media(&small), 308);
        assert_eq!(Tokenizer::OpenAi.media(&large), 12_000);
        assert_eq!(Tokenizer::Gemini.media(&large), 1_120);

        let dir = std::env::temp_dir().join(format!("haarness-tokens-{}", Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("shot.png");
        std::fs::write(&path, png(800, 600)).unwrap();
        assert_eq!(Tokenizer::Claude.media(path.to_str().unwrap()), 641);
        assert_eq!(
            Tokenizer::Claude.media(&format!("file://{}", path.display())),
            641
        );
        // Unreadable sources fall back to a large image rather than zero.
        assert_eq!(
            Tokenizer::ClaudeLegacy.media(dir.join("missing.png").to_str().unwrap()),
            1_603
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn other_image_formats_and_pdfs_are_measured() {
        let gif = [
            b"GIF89a".as_slice(),
            &640_u16.to_le_bytes(),
            &480_u16.to_le_bytes(),
        ]
        .concat();
        assert_eq!(image_size(&gif), Some((640, 480)));
        let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xE1, 0x00, 0x04, 0x00, 0x00];
        jpeg.extend([0xFF, 0xC0, 0x00, 0x11, 0x08]);
        jpeg.extend(1_080_u16.to_be_bytes());
        jpeg.extend(1_920_u16.to_be_bytes());
        jpeg.extend([3, 0, 0, 0, 0]);
        assert_eq!(image_size(&jpeg), Some((1_920, 1_080)));
        let mut webp = b"RIFF\0\0\0\0WEBPVP8X\0\0\0\0\0\0\0\0".to_vec();
        webp.extend([0xFF, 0x03, 0x00, 0x57, 0x02, 0x00]);
        assert_eq!(image_size(&webp), Some((1_024, 600)));
        let pdf = b"%PDF-1.7\n1 0 obj << /Type /Pages /Count 2 >>\n2 0 obj << /Type /Page >>\n3 0 obj <</Type/Page>>";
        let source = format!("data:application/pdf;base64,{}", base64(pdf));
        assert_eq!(Tokenizer::Claude.media(&source), 6_000);
        assert_eq!(Tokenizer::Gemini.media(&source), 1_200);
    }

    #[test]
    fn context_is_the_last_report_plus_what_was_appended() {
        let result = |output: &str| ItemData::ToolResult {
            call_id: "c".into(),
            name: "read".into(),
            output: output.into(),
            is_error: false,
            metadata: json!(null),
        };
        let items = vec![
            item(1, user("hello")),
            item(2, ItemData::AssistantMessage { text: "hi".into() }),
            item(3, usage(10_000, 500, None)),
            item(4, result(&"word ".repeat(400))),
        ];
        let context = context(&items, Tokenizer::OpenAi);
        assert_eq!(
            context.reported, 10_500,
            "the round's output is sent back too"
        );
        let appended = Tokenizer::OpenAi.item(&items[3].data);
        assert_eq!(context.estimated, appended);
        assert_eq!(context.tokens, 10_500 + appended);
    }

    #[test]
    fn reports_from_before_a_compaction_never_anchor_it() {
        let items = vec![
            item(1, user("old question")),
            item(2, usage(150_000, 2_000, None)),
            item(3, user("recent question")),
            item(
                4,
                ItemData::Compaction {
                    summary: "We fixed the parser.".into(),
                    files_touched: Vec::new(),
                    outstanding: Vec::new(),
                    todo_checkpoint: None,
                    tail_start_sequence: Some(2),
                },
            ),
        ];
        let context = context(&items, Tokenizer::Claude);
        assert_eq!(context.reported, 0);
        assert!(context.tokens < 200, "{context:?}");

        let mut items = items;
        items.push(item(5, usage(3_000, 100, None)));
        assert_eq!(super::context(&items, Tokenizer::Claude).reported, 3_100);
    }

    #[test]
    fn estimates_and_empty_reports_never_anchor_the_context() {
        let estimated = |input_tokens| ItemData::Usage {
            prompt: None,
            input_tokens,
            output_tokens: 5,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            cost_usd: None,
            estimated: true,
            first_token_ms: None,
            stream_ms: None,
            stream_tokens: None,
        };
        let items = vec![
            item(1, user("hello")),
            item(2, usage(9_000, 100, None)),
            item(3, user("again")),
            item(4, estimated(50_000)),
            item(5, usage(0, 0, None)),
        ];
        let context = context(&items, Tokenizer::Claude);
        assert_eq!(context.reported, 9_100, "{context:?}");
        assert!(context.estimated < 50, "{context:?}");
    }

    #[test]
    fn calibration_compares_the_growth_of_history_between_reports() {
        let prompt = |history_tokens| PromptUsage {
            system_tokens: 1_000,
            tool_tokens: 500,
            history_tokens,
            media_attachments: 0,
            media_tokens: 0,
        };
        // The whole requests compare at 0.68 and 0.82 (tool schemas cost less
        // than their JSON); the 2,000 estimated tokens of history cost 2,400.
        let items = vec![
            item(1, usage(1_700, 10, Some(prompt(1_000)))),
            item(2, user("question")),
            item(3, usage(4_100, 10, Some(prompt(3_000)))),
            item(4, user(&"word ".repeat(1_000))),
        ];
        assert!((calibration(&items) - 1.2).abs() < 1e-9);
        let context = context(&items, Tokenizer::OpenAi);
        #[allow(
            clippy::cast_precision_loss,
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss
        )]
        let expected = (Tokenizer::OpenAi.item(&items[3].data) as f64 * 1.2).ceil() as u64;
        assert_eq!(context.estimated, expected);

        // Too little growth, a compaction in between, older records without
        // media estimates: no calibration.
        let close = vec![
            item(1, usage(1_700, 0, Some(prompt(1_000)))),
            item(2, usage(2_300, 0, Some(prompt(1_500)))),
        ];
        assert!((calibration(&close) - 1.0).abs() < 1e-9);
        let compacted = vec![
            item(1, usage(1_700, 0, Some(prompt(1_000)))),
            item(
                2,
                ItemData::Compaction {
                    summary: String::new(),
                    files_touched: Vec::new(),
                    outstanding: Vec::new(),
                    todo_checkpoint: None,
                    tail_start_sequence: None,
                },
            ),
            item(3, usage(4_100, 0, Some(prompt(3_000)))),
        ];
        assert!((calibration(&compacted) - 1.0).abs() < 1e-9);
        let legacy = PromptUsage {
            media_attachments: 2,
            ..prompt(3_000)
        };
        let old = vec![
            item(1, usage(1_700, 0, Some(prompt(1_000)))),
            item(2, usage(9_000, 0, Some(legacy))),
        ];
        assert!((calibration(&old) - 1.0).abs() < 1e-9);
        // Far-off ratios are clamped.
        let wild = vec![
            item(1, usage(1_700, 0, Some(prompt(1_000)))),
            item(2, usage(100_000, 0, Some(prompt(3_000)))),
        ];
        assert!((calibration(&wild) - 1.5).abs() < 1e-9);
    }

    #[test]
    fn without_a_report_the_last_system_prompt_and_tools_are_counted() {
        let prompt = PromptUsage {
            system_tokens: 4_000,
            tool_tokens: 6_000,
            history_tokens: 10,
            media_attachments: 0,
            media_tokens: 0,
        };
        let items = vec![
            item(1, usage(10_010, 5, Some(prompt))),
            item(
                2,
                ItemData::Compaction {
                    summary: "Summary.".into(),
                    files_touched: Vec::new(),
                    outstanding: Vec::new(),
                    todo_checkpoint: None,
                    tail_start_sequence: None,
                },
            ),
        ];
        let estimate = context(&items, Tokenizer::Claude);
        assert_eq!(estimate.reported, 0);
        assert!(estimate.estimated > 10_000, "{estimate:?}");

        // Scaled like the whole last request: its tools cost 0.6 of their JSON.
        let mut items = items;
        let ItemData::Usage { prompt, .. } = items[0].data.clone() else {
            unreachable!()
        };
        items[0].data = usage(6_006, 5, prompt);
        let estimate = context(&items, Tokenizer::Claude);
        assert!((6_000..6_100).contains(&estimate.estimated), "{estimate:?}");
    }
}

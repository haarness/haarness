//! The images of one request, held to what its provider takes.
//!
//! A chat with many screenshots grows past what a provider accepts in one
//! request: too many bytes (a 413) or too many images. Refused, the request
//! would fail on every later turn, and leaving out the image that request
//! added would hide each new screenshot in turn. So before a request is sent,
//! once its images pass the provider's [`ImageLimits`], the oldest are
//! replaced with a short note until the rest fit in three fifths of them: the
//! newest images always reach the model.
//!
//! The cut is a replay over the images in order: each one joins the kept
//! total, and whenever that total passes a limit the oldest are dropped down
//! to the lower mark. A new image moves the cut only when the kept images
//! pass a limit again, not on every request, so the replaced prefix, and the
//! provider's prompt cache with it, stays the same between cuts. A cut never
//! takes what the person attached to their messages in the turn it happens
//! in; when those alone pass a limit the request goes as it is, and a refusal
//! is handled as before. That turn is the one the image that passed the limit
//! belongs to, as the history records it, not the turn now running, so a new
//! turn replays every earlier cut the same way and its first request reads
//! the cache too. Stored messages are never changed.

use std::collections::HashSet;

use haarness_protocol::{Item, ItemData, TurnId};

use crate::{ImageLimits, ProviderMessage};

/// Where a provider message came from in the thread.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Origin {
    /// The turn its item belongs to.
    pub turn: Option<TurnId>,
    /// Written by the person (a steer they queued included), not made by
    /// Haarness for them.
    pub person: bool,
}

/// What a cut left out of one request, for its log line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Cut {
    /// Images in the request before the cut.
    pub images: usize,
    /// Images replaced by a note.
    pub removed: usize,
    /// Base64 bytes of all the images.
    pub bytes: usize,
    /// Base64 bytes of the images kept.
    pub kept: usize,
}

/// The attachments of the person's messages in `turn`, which a refusal leaves
/// out last.
pub(crate) fn turn_attachments(items: &[Item], turn: Option<TurnId>) -> HashSet<String> {
    items
        .iter()
        .filter(|item| turn.is_some() && item.turn_id == turn)
        .filter_map(|item| match &item.data {
            ItemData::UserMessage { attachments, .. } => Some(attachments.iter().cloned()),
            _ => None,
        })
        .flatten()
        .collect()
}

/// Replaces the oldest images of `messages` with a note once they pass
/// `limits` (see the module documentation). `origins` says where each
/// message came from. `None` when every image stays.
pub(crate) fn bound(
    messages: &mut [ProviderMessage],
    origins: &[Origin],
    limits: ImageLimits,
) -> Option<Cut> {
    struct Image {
        message: usize,
        source: String,
        bytes: usize,
    }
    debug_assert_eq!(messages.len(), origins.len());
    let origin = |message: usize| origins.get(message).copied().unwrap_or_default();
    let mut images = Vec::new();
    for (index, message) in messages.iter().enumerate() {
        let attachments = match message {
            // Adapters drop developer-role messages, and their images.
            ProviderMessage::Media {
                role, attachments, ..
            } if role != "developer" => attachments,
            ProviderMessage::ToolResult { attachments, .. } => attachments,
            _ => continue,
        };
        for source in attachments {
            if crate::attachments::is_image_source(source) {
                images.push(Image {
                    message: index,
                    source: source.clone(),
                    bytes: crate::attachments::sent_bytes(source),
                });
            }
        }
    }
    let over = |bytes: usize, count: usize, (most_bytes, most_count): (usize, Option<usize>)| {
        bytes > most_bytes || most_count.is_some_and(|most| count > most)
    };
    let limit = (limits.bytes, limits.count);
    let target = (
        limits.bytes / 5 * 3,
        limits.count.map(|count| count.saturating_mul(3) / 5),
    );
    let mut dropped = vec![false; images.len()];
    // The images still sent, oldest first.
    let mut sent = Vec::new();
    let (mut kept, mut count) = (0_usize, 0_usize);
    for (index, image) in images.iter().enumerate() {
        sent.push(index);
        kept = kept.saturating_add(image.bytes);
        count += 1;
        if !over(kept, count, limit) {
            continue;
        }
        // The person's own images in the turn this image belongs to stay.
        let turn = origin(image.message).turn;
        let own = |message: usize| {
            let origin = origin(message);
            origin.person && turn.is_some() && origin.turn == turn
        };
        let mut position = 0;
        while over(kept, count, target) && position < sent.len() {
            let oldest = &images[sent[position]];
            if own(oldest.message) {
                position += 1;
                continue;
            }
            dropped[sent.remove(position)] = true;
            kept -= oldest.bytes;
            count -= 1;
        }
    }
    let removed = dropped.iter().filter(|dropped| **dropped).count();
    if removed == 0 {
        return None;
    }
    let bytes = images.iter().map(|image| image.bytes).sum();
    for (image, _) in images.iter().zip(&dropped).filter(|(_, dropped)| **dropped) {
        replace(&mut messages[image.message], &image.source);
    }
    Some(Cut {
        images: images.len(),
        removed,
        bytes,
        kept,
    })
}

/// Takes the first `source` out of `message`, naming it in the text instead.
/// A person's message left without media is sent as text.
fn replace(message: &mut ProviderMessage, source: &str) {
    use std::fmt::Write as _;
    let (text, attachments) = match message {
        ProviderMessage::Media {
            content,
            attachments,
            ..
        } => (content, attachments),
        ProviderMessage::ToolResult {
            output,
            attachments,
            ..
        } => (output, attachments),
        _ => return,
    };
    let Some(position) = attachments.iter().position(|kept| kept == source) else {
        return;
    };
    attachments.remove(position);
    // One line the model reads: what is gone, why, and how to see it again.
    let _ = match crate::attachments::file_path(source) {
        Some(path) => write!(
            text,
            "\n[Image {} omitted to keep the request within the provider's limits. It is no longer visible: do not describe it from memory, and read the file again if it is needed.]",
            path.display()
        ),
        None => write!(
            text,
            "\n[Image {} omitted to keep the request within the provider's limits. It is no longer visible: do not describe it from memory, and ask the person to attach it again if it is needed.]",
            crate::attachments::display_name(source)
        ),
    };
    if let ProviderMessage::Media {
        role,
        content,
        attachments,
    } = message
        && attachments.is_empty()
    {
        *message = ProviderMessage::Text {
            role: std::mem::take(role),
            content: std::mem::take(content),
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: usize = 1024 * 1024;

    /// A data URL named `name` whose base64 payload is `bytes` long.
    fn image(name: &str, bytes: usize) -> String {
        format!(
            "data:image/png;name={name}.png;base64,{}",
            "A".repeat(bytes)
        )
    }

    fn photo(name: &str, bytes: usize) -> ProviderMessage {
        ProviderMessage::Media {
            role: "user".into(),
            content: format!("photo {name}"),
            attachments: vec![image(name, bytes)],
        }
    }

    fn shot(name: &str, bytes: usize) -> [ProviderMessage; 2] {
        [
            ProviderMessage::ToolCall {
                call_id: name.into(),
                name: "screenshot".into(),
                arguments: serde_json::json!({}),
                provider_metadata: serde_json::Value::Null,
            },
            ProviderMessage::ToolResult {
                call_id: name.into(),
                name: "screenshot".into(),
                output: format!("shot {name}"),
                is_error: false,
                attachments: vec![image(name, bytes)],
            },
        ]
    }

    /// The names of the images still sent, in order.
    fn sent(messages: &[ProviderMessage]) -> Vec<String> {
        messages
            .iter()
            .flat_map(|message| match message {
                ProviderMessage::Media { attachments, .. }
                | ProviderMessage::ToolResult { attachments, .. } => attachments.clone(),
                _ => Vec::new(),
            })
            .map(|source| crate::attachments::display_name(&source).replace(".png", ""))
            .collect()
    }

    fn noted(messages: &[ProviderMessage]) -> usize {
        serde_json::to_string(messages)
            .unwrap()
            .matches("omitted to keep the request within the provider's limits")
            .count()
    }

    /// The origins of a chat in which each user message a person wrote
    /// opens a turn, and what follows it belongs to that turn.
    fn turns(messages: &[ProviderMessage]) -> Vec<Origin> {
        let mut turn = None;
        messages
            .iter()
            .map(|message| {
                let person = matches!(message,
                    ProviderMessage::Text { role, content } if role == "user" && !crate::turn_context::is_turn_reminder(content))
                    || matches!(message, ProviderMessage::Media { role, .. } if role == "user");
                if person {
                    turn = Some(TurnId::new());
                }
                Origin { turn, person }
            })
            .collect()
    }

    fn bounded(
        mut messages: Vec<ProviderMessage>,
        limits: ImageLimits,
    ) -> (Vec<ProviderMessage>, Option<Cut>) {
        let origins = turns(&messages);
        let cut = bound(&mut messages, &origins, limits);
        (messages, cut)
    }

    /// A chat whose messages say which turn they belong to and who wrote
    /// them, as `provider_messages_with_origins` records it.
    #[derive(Default)]
    struct Chat {
        messages: Vec<ProviderMessage>,
        origins: Vec<Origin>,
        turn: Option<TurnId>,
    }

    impl Chat {
        /// A message the person wrote, opening a new turn.
        fn opens(mut self, message: ProviderMessage) -> Self {
            self.turn = Some(TurnId::new());
            self.add(message, true)
        }

        fn add(mut self, message: ProviderMessage, person: bool) -> Self {
            self.messages.push(message);
            self.origins.push(Origin {
                turn: self.turn,
                person,
            });
            self
        }

        fn shots(mut self, names: &[&str], bytes: usize) -> Self {
            for name in names {
                for message in shot(name, bytes) {
                    self = self.add(message, false);
                }
            }
            self
        }

        fn bounded(mut self, limits: ImageLimits) -> Vec<String> {
            bound(&mut self.messages, &self.origins, limits);
            sent(&self.messages)
        }
    }

    fn user(text: &str) -> ProviderMessage {
        ProviderMessage::Text {
            role: "user".into(),
            content: text.into(),
        }
    }

    const REMINDER: &str =
        "<system-reminder>\nToday's date is now Saturday, 24 October 2026.\n</system-reminder>";

    #[test]
    fn the_persons_own_image_stays_whatever_comes_between_it_and_the_cut() {
        let design = || ProviderMessage::Media {
            role: "user".into(),
            content: "match this design".into(),
            attachments: vec![image("design", 8 * MIB)],
        };
        // A steer the person queued while the model worked.
        let steered = Chat::default()
            .opens(design())
            .add(user(REMINDER), false)
            .shots(&["s1", "s2"], 4 * MIB)
            .add(
                user("<system-reminder>\nThe user sent the following message while you were working:\nalso the footer\n</system-reminder>"),
                true,
            )
            .shots(&["s3", "s4", "s5"], 4 * MIB);
        assert_eq!(steered.bounded(ImageLimits::default()), ["design", "s5"]);
        // A child agent's result delivered into the turn.
        let delegated = Chat::default()
            .opens(design())
            .add(user(REMINDER), false)
            .shots(&["s1"], 5 * MIB)
            .add(
                user("<task-result>the child agent finished</task-result>"),
                false,
            )
            .shots(&["s2", "s3", "s4"], 5 * MIB);
        assert_eq!(delegated.bounded(ImageLimits::default()), ["design", "s4"]);
        // A compaction summary, which is a user message too.
        let compacted = Chat::default()
            .opens(design())
            .add(
                user("<compaction-summary>\nwhat happened so far\n</compaction-summary>"),
                false,
            )
            .shots(&["s1", "s2", "s3", "s4", "s5"], 4 * MIB);
        assert_eq!(compacted.bounded(ImageLimits::default()), ["design", "s5"]);
    }

    fn photos(names: &str, bytes: usize) -> Vec<ProviderMessage> {
        names
            .chars()
            .map(|name| photo(&name.to_string(), bytes))
            .collect()
    }

    #[test]
    fn a_request_within_the_limits_is_sent_as_it_is() {
        let messages = photos("abc", 8 * MIB);
        let (bounded, cut) = bounded(messages.clone(), ImageLimits::default());
        assert_eq!(cut, None);
        assert_eq!(
            serde_json::to_string(&bounded).unwrap(),
            serde_json::to_string(&messages).unwrap()
        );
    }

    #[test]
    fn past_the_byte_limit_the_oldest_are_noted_and_the_newest_sent() {
        // 6 MiB each: 30 MiB passes 25, and leaving out the oldest three
        // leaves 12 MiB, under the 15 MiB mark.
        let (messages, cut) = bounded(photos("abcde", 6 * MIB), ImageLimits::default());
        assert_eq!(sent(&messages), ["d", "e"]);
        assert_eq!(
            cut,
            Some(Cut {
                images: 5,
                removed: 3,
                bytes: 30 * MIB,
                kept: 12 * MIB,
            })
        );
        // A person's message left without its image is text naming it.
        let ProviderMessage::Text { role, content } = &messages[0] else {
            panic!("{:?}", messages[0]);
        };
        assert_eq!(role, "user");
        assert_eq!(
            content,
            "photo a\n[Image a.png omitted to keep the request within the provider's limits. It is no longer visible: do not describe it from memory, and ask the person to attach it again if it is needed.]"
        );
        assert!(
            matches!(&messages[3], ProviderMessage::Media { content, .. } if content == "photo d")
        );
    }

    #[test]
    fn past_the_image_count_the_oldest_are_noted_down_to_three_fifths() {
        let limits = ImageLimits {
            count: Some(20),
            ..ImageLimits::default()
        };
        let names = "abcdefghijklmnopqrstu";
        let (messages, _) = bounded(photos(names, MIB / 100), limits);
        // 21 images pass 20; the newest 12 remain.
        assert_eq!(sent(&messages).concat(), "jklmnopqrstu");
        assert_eq!(noted(&messages), 9);
        let (messages, cut) = bounded(photos(&names[..20], MIB / 100), limits);
        assert_eq!(cut, None);
        assert_eq!(sent(&messages).len(), 20);
    }

    #[test]
    fn the_cut_stays_put_until_the_kept_images_pass_the_limit_again() {
        let history = || {
            let mut messages = photos("abcde", 6 * MIB);
            messages.push(ProviderMessage::Text {
                role: "assistant".into(),
                content: "ok".into(),
            });
            messages
        };
        let (first, _) = bounded(history(), ImageLimits::default());
        assert_eq!(sent(&first), ["d", "e"]);
        // Every later request carries the same noted prefix, byte for byte:
        // that prefix is the provider's prompt cache.
        let mut later = history();
        later.push(photo("f", 6 * MIB));
        later.extend(shot("g", 6 * MIB));
        let (later, _) = bounded(later, ImageLimits::default());
        assert_eq!(sent(&later), ["d", "e", "f", "g"]);
        assert_eq!(
            serde_json::to_string(&later[..first.len()]).unwrap(),
            serde_json::to_string(&first).unwrap()
        );
        // 12 + 6 + 6 = 24 MiB is under 25; the next image moves the cut.
        let mut moved = history();
        moved.push(photo("f", 6 * MIB));
        moved.extend(shot("g", 6 * MIB));
        moved.push(photo("h", 6 * MIB));
        let (moved, _) = bounded(moved, ImageLimits::default());
        assert_eq!(sent(&moved), ["g", "h"]);
    }

    #[test]
    fn the_images_of_the_turns_own_message_always_go() {
        // The person attached two screenshots, then the model took four of
        // its own. Past 25 MiB the cut takes the history and then the oldest
        // tool image: the person's two stay, though they are older.
        let mut messages = photos("ab", 6 * MIB);
        messages.push(ProviderMessage::Media {
            role: "user".into(),
            content: "look at these".into(),
            attachments: vec![image("c", 3 * MIB), image("d", 3 * MIB)],
        });
        for name in ["e", "f", "g", "h"] {
            messages.extend(shot(name, 5 * MIB));
        }
        let origins = turns(&messages);
        bound(&mut messages, &origins, ImageLimits::default());
        assert_eq!(sent(&messages), ["c", "d", "f", "g", "h"]);

        // A message that alone passes the limit goes as it is.
        let alone = (0..5).map(|index| image(&index.to_string(), 6 * MIB));
        let mut messages = vec![ProviderMessage::Media {
            role: "user".into(),
            content: "all of these".into(),
            attachments: alone.collect(),
        }];
        let origins = turns(&messages);
        assert_eq!(bound(&mut messages, &origins, ImageLimits::default()), None);
        assert_eq!(sent(&messages).len(), 5);
    }

    #[test]
    fn a_new_turn_replays_the_cuts_before_it_and_reads_the_cache() {
        // In the first turn the person attached a photo and the model took
        // four screenshots. Past 25 MiB the cut kept the photo and took the
        // older screenshots.
        let mut first = vec![ProviderMessage::Media {
            role: "user".into(),
            content: "look".into(),
            attachments: vec![image("photo", 6 * MIB)],
        }];
        // The hidden context stored with a turn is a message of its own.
        first.push(ProviderMessage::Text {
            role: "user".into(),
            content: "<system-reminder>\nToday's date is now Saturday, 24 October 2026.\n</system-reminder>".into(),
        });
        for name in ["s1", "s2", "s3", "s4"] {
            first.extend(shot(name, 6 * MIB));
        }
        let (sent_first, _) = bounded(first.clone(), ImageLimits::default());
        assert_eq!(sent(&sent_first), ["photo", "s4"]);
        // The next turn's request carries that history unchanged, though the
        // photo is no longer the turn's own.
        let mut next = first;
        next.push(ProviderMessage::Text {
            role: "assistant".into(),
            content: "done".into(),
        });
        next.push(photo("new", MIB));
        let (sent_next, _) = bounded(next, ImageLimits::default());
        assert_eq!(sent(&sent_next), ["photo", "s4", "new"]);
        assert_eq!(
            serde_json::to_string(&sent_next[..sent_first.len()]).unwrap(),
            serde_json::to_string(&sent_first).unwrap()
        );
    }

    #[test]
    fn only_images_count_and_only_images_are_noted() {
        // A PDF is neither counted nor replaced, however large.
        let pdf = format!("data:application/pdf;base64,{}", "A".repeat(30 * MIB));
        let mut messages = vec![ProviderMessage::Media {
            role: "user".into(),
            content: "report".into(),
            attachments: vec![pdf.clone(), image("a", 6 * MIB)],
        }];
        messages.extend(photos("bcd", 6 * MIB));
        let (messages, cut) = bounded(messages, ImageLimits::default());
        assert_eq!(cut, None);
        assert!(
            matches!(&messages[0], ProviderMessage::Media { attachments, .. } if attachments[0] == pdf)
        );

        let mut messages = vec![ProviderMessage::Media {
            role: "user".into(),
            content: "report".into(),
            attachments: vec![pdf.clone(), image("a", 6 * MIB)],
        }];
        messages.extend(photos("bcde", 6 * MIB));
        let (messages, _) = bounded(messages, ImageLimits::default());
        // The PDF stays where it was; the image beside it is noted.
        assert!(
            matches!(&messages[0], ProviderMessage::Media { attachments, content, .. }
            if attachments == std::slice::from_ref(&pdf) && content.contains("[Image a.png omitted"))
        );
        assert_eq!(sent(&messages)[1..], ["d", "e"]);
    }

    #[test]
    fn an_image_file_counts_as_the_base64_of_its_bytes() {
        let dir = std::env::temp_dir().join(format!("haarness-budget-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("big.png");
        // 4.5 MiB of bytes are 6 MiB of base64.
        std::fs::write(&file, vec![0_u8; 9 * MIB / 2]).unwrap();
        let path = file.to_string_lossy().into_owned();
        assert_eq!(crate::attachments::sent_bytes(&path), 6 * MIB);
        let mut messages = vec![ProviderMessage::Media {
            role: "user".into(),
            content: "file".into(),
            attachments: vec![path.clone()],
        }];
        messages.extend(photos("bcde", 6 * MIB));
        let (messages, _) = bounded(messages, ImageLimits::default());
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(
            matches!(&messages[0], ProviderMessage::Text { content, .. } if content.ends_with(&format!("[Image {path} omitted to keep the request within the provider's limits. It is no longer visible: do not describe it from memory, and read the file again if it is needed.]")))
        );
        assert_eq!(sent(&messages), ["d", "e"]);
    }

    #[test]
    fn the_turns_attachments_are_its_person_messages() {
        let turn = TurnId::new();
        let item = |turn_id, attachments: Vec<&str>| Item {
            id: haarness_protocol::ItemId::new(),
            thread_id: haarness_protocol::ThreadId::new(),
            turn_id,
            sequence: 1,
            data: ItemData::UserMessage {
                text: "look".into(),
                attachments: attachments.into_iter().map(str::to_owned).collect(),
                queued: false,
                synthetic: false,
                command_line: None,
            },
            created_at: chrono::Utc::now(),
        };
        let items = [
            item(Some(TurnId::new()), vec!["/old.png"]),
            item(Some(turn), vec!["/new.png"]),
            item(None, vec!["/loose.png"]),
        ];
        assert_eq!(
            turn_attachments(&items, Some(turn)),
            HashSet::from(["/new.png".to_owned()])
        );
        assert!(turn_attachments(&items, None).is_empty());
    }
}

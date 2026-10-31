//! AWS's binary event-stream framing, which Bedrock's ConverseStream answers
//! in: each message is a length prelude, typed headers, a payload and two
//! CRC32 checksums. Only string headers are kept; the others carry nothing a
//! Converse event needs.

use std::collections::BTreeMap;

/// One decoded message: its string headers (`:event-type`, `:message-type`,
/// `:exception-type`) and its payload, JSON for every Converse event.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Message {
    pub headers: BTreeMap<String, String>,
    pub payload: Vec<u8>,
}

impl Message {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(String::as_str)
    }
}

/// Frames read so far; bytes are pushed as they arrive and whole messages
/// taken out.
#[derive(Default)]
pub(crate) struct Decoder {
    buffer: Vec<u8>,
}

/// The prelude (total length, headers length, prelude CRC) and the message CRC.
const PRELUDE: usize = 12;
const TRAILER: usize = 4;
/// AWS caps a message at 16 MiB; anything longer is a corrupt length.
const LONGEST: usize = 16 * 1024 * 1024;

impl Decoder {
    pub fn push(&mut self, bytes: &[u8]) {
        self.buffer.extend_from_slice(bytes);
    }

    /// The next whole message, `None` until one has arrived, or an error for
    /// framing that cannot be read (after which the stream is unusable).
    pub fn next(&mut self) -> Option<Result<Message, String>> {
        if self.buffer.len() < PRELUDE {
            return None;
        }
        let total = u32_at(&self.buffer, 0) as usize;
        let headers_len = u32_at(&self.buffer, 4) as usize;
        if crc32(&self.buffer[..8]) != u32_at(&self.buffer, 8) {
            return Some(Err("event stream prelude checksum mismatch".into()));
        }
        if total < PRELUDE + TRAILER + headers_len || total > LONGEST {
            return Some(Err(format!(
                "event stream message length {total} is invalid"
            )));
        }
        if self.buffer.len() < total {
            return None;
        }
        let frame: Vec<u8> = self.buffer.drain(..total).collect();
        if crc32(&frame[..total - TRAILER]) != u32_at(&frame, total - TRAILER) {
            return Some(Err("event stream message checksum mismatch".into()));
        }
        let headers = match headers(&frame[PRELUDE..PRELUDE + headers_len]) {
            Ok(headers) => headers,
            Err(error) => return Some(Err(error)),
        };
        Some(Ok(Message {
            headers,
            payload: frame[PRELUDE + headers_len..total - TRAILER].to_vec(),
        }))
    }

    /// Whether bytes of an unfinished message are still held.
    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

fn headers(mut bytes: &[u8]) -> Result<BTreeMap<String, String>, String> {
    let truncated = || "event stream header is truncated".to_owned();
    let mut headers = BTreeMap::new();
    while !bytes.is_empty() {
        let name_len = *bytes.first().ok_or_else(truncated)? as usize;
        let name = bytes.get(1..1 + name_len).ok_or_else(truncated)?;
        let name = String::from_utf8_lossy(name).into_owned();
        let kind = *bytes.get(1 + name_len).ok_or_else(truncated)?;
        bytes = &bytes[2 + name_len..];
        // Value sizes by type: bool true/false, byte, short, int, long,
        // byte array, string, timestamp, UUID.
        let size = match kind {
            0 | 1 => 0,
            2 => 1,
            3 => 2,
            4 => 4,
            5 | 8 => 8,
            9 => 16,
            6 | 7 => {
                let len = bytes.get(..2).ok_or_else(truncated)?;
                2 + u16::from_be_bytes([len[0], len[1]]) as usize
            }
            other => return Err(format!("event stream header type {other} is unknown")),
        };
        let value = bytes.get(..size).ok_or_else(truncated)?;
        if kind == 7 {
            headers.insert(name, String::from_utf8_lossy(&value[2..]).into_owned());
        }
        bytes = &bytes[size..];
    }
    Ok(headers)
}

/// CRC-32 (IEEE), as the framing uses.
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xEDB8_8320 & (crc & 1).wrapping_neg());
        }
    }
    !crc
}

/// One message framed as AWS frames it, for fixtures.
#[cfg(test)]
pub(crate) fn encode(headers: &[(&str, &str)], payload: &[u8]) -> Vec<u8> {
    let mut encoded_headers = Vec::new();
    for (name, value) in headers {
        encoded_headers.push(u8::try_from(name.len()).unwrap());
        encoded_headers.extend_from_slice(name.as_bytes());
        encoded_headers.push(7);
        encoded_headers.extend_from_slice(&u16::try_from(value.len()).unwrap().to_be_bytes());
        encoded_headers.extend_from_slice(value.as_bytes());
    }
    let total = PRELUDE + encoded_headers.len() + payload.len() + TRAILER;
    let mut message = Vec::with_capacity(total);
    message.extend_from_slice(&u32::try_from(total).unwrap().to_be_bytes());
    message.extend_from_slice(&u32::try_from(encoded_headers.len()).unwrap().to_be_bytes());
    let prelude_crc = crc32(&message);
    message.extend_from_slice(&prelude_crc.to_be_bytes());
    message.extend_from_slice(&encoded_headers);
    message.extend_from_slice(payload);
    let crc = crc32(&message);
    message.extend_from_slice(&crc.to_be_bytes());
    message
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_checksum_is_crc32_ieee() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn messages_split_anywhere_decode_whole_and_in_order() {
        let first = encode(
            &[
                (":message-type", "event"),
                (":event-type", "contentBlockDelta"),
            ],
            br#"{"delta":{"text":"Hel"}}"#,
        );
        let second = encode(
            &[(":message-type", "event"), (":event-type", "messageStop")],
            br#"{"stopReason":"end_turn"}"#,
        );
        let bytes = [first, second].concat();
        let mut decoder = Decoder::default();
        let mut messages = Vec::new();
        for chunk in bytes.chunks(7) {
            decoder.push(chunk);
            while let Some(message) = decoder.next() {
                messages.push(message.unwrap());
            }
        }
        assert!(decoder.is_empty());
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].header(":event-type"), Some("contentBlockDelta"));
        assert_eq!(messages[0].payload, br#"{"delta":{"text":"Hel"}}"#);
        assert_eq!(messages[1].header(":event-type"), Some("messageStop"));
    }

    #[test]
    fn a_corrupt_message_is_an_error_not_a_guess() {
        let mut bytes = encode(&[(":event-type", "messageStop")], b"{}");
        let last = bytes.len() - 6;
        bytes[last] ^= 0xFF;
        let mut decoder = Decoder::default();
        decoder.push(&bytes);
        assert!(decoder.next().unwrap().is_err());
    }
}

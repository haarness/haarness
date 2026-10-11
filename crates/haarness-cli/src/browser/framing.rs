//! Chromium native messaging: each message is a 4-byte little-endian length
//! and that many bytes of UTF-8 JSON. Chrome closes the port on a message to
//! the extension above 1 MB, so a larger one goes out as consecutive `chunk`
//! frames of at most `CHUNK_BYTES` of its JSON each, which the extension joins
//! back into bytes before decoding once.
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::io;
use tokio::{
    io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _},
    sync::mpsc,
};
use uuid::Uuid;

/// The most Chrome sends the host in one message.
pub(crate) const MAX_INBOUND: usize = 64 * 1024 * 1024;
/// JSON bytes per chunk: base64 makes about 933 KB of it, under Chrome's 1 MB.
pub(crate) const CHUNK_BYTES: usize = 700_000;

/// The next message, or `None` when Chrome closed stdin between messages.
pub(crate) async fn read_frame<R: AsyncRead + Unpin>(
    reader: &mut R,
) -> io::Result<Option<Vec<u8>>> {
    let mut header = [0_u8; 4];
    let mut filled = 0;
    while filled < header.len() {
        let read = reader.read(&mut header[filled..]).await?;
        if read == 0 {
            return if filled == 0 {
                Ok(None)
            } else {
                Err(io::ErrorKind::UnexpectedEof.into())
            };
        }
        filled += read;
    }
    let length = usize::try_from(u32::from_le_bytes(header)).unwrap_or(usize::MAX);
    if length > MAX_INBOUND {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("a {length}-byte message is over the {MAX_INBOUND}-byte limit"),
        ));
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).await?;
    Ok(Some(body))
}

/// The frames that carry `message`: one, or its chunks in order.
pub(crate) fn encode(message: &Value) -> Vec<Vec<u8>> {
    let payload = serde_json::to_vec(message).expect("a JSON value always serialises");
    if payload.len() <= CHUNK_BYTES {
        return vec![frame(&payload)];
    }
    let id = Uuid::now_v7().to_string();
    let count = payload.len().div_ceil(CHUNK_BYTES);
    payload
        .chunks(CHUNK_BYTES)
        .enumerate()
        .map(|(index, part)| {
            let chunk = json!({
                "type": "chunk",
                "id": id,
                "index": index,
                "count": count,
                "data": STANDARD.encode(part),
            });
            frame(&serde_json::to_vec(&chunk).expect("a JSON value always serialises"))
        })
        .collect()
}

fn frame(payload: &[u8]) -> Vec<u8> {
    let length = u32::try_from(payload.len()).expect("outbound frames stay under 1 MB");
    let mut frame = Vec::with_capacity(4 + payload.len());
    frame.extend_from_slice(&length.to_le_bytes());
    frame.extend_from_slice(payload);
    frame
}

/// The only writer of the extension's port: every frame of one message goes
/// out before the next message starts, so a `cancel` never lands inside a
/// chunked command. Ends when every sender is gone or Chrome stops reading.
pub(crate) async fn write_messages<W: AsyncWrite + Unpin>(
    mut writer: W,
    mut messages: mpsc::UnboundedReceiver<Value>,
) -> io::Result<()> {
    while let Some(message) = messages.recv().await {
        for frame in encode(&message) {
            writer.write_all(&frame).await?;
        }
        writer.flush().await?;
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Reads one logical message the way the extension does: chunks are
    /// joined as bytes, then decoded once.
    pub(crate) async fn read_message<R: AsyncRead + Unpin>(reader: &mut R) -> Option<Value> {
        let first: Value = serde_json::from_slice(&read_frame(reader).await.ok()??).ok()?;
        if first["type"] != "chunk" {
            return Some(first);
        }
        let count = first["count"].as_u64()?;
        let id = first["id"].clone();
        let mut bytes = STANDARD.decode(first["data"].as_str()?).ok()?;
        for index in 1..count {
            let next: Value = serde_json::from_slice(&read_frame(reader).await.ok()??).ok()?;
            assert_eq!(next["type"], "chunk");
            assert_eq!(next["id"], id, "chunks of one message are contiguous");
            assert_eq!(next["index"], index);
            bytes.extend(STANDARD.decode(next["data"].as_str()?).ok()?);
        }
        Some(serde_json::from_slice(&bytes).unwrap())
    }

    fn frames_of(message: &Value) -> Vec<Value> {
        encode(message)
            .into_iter()
            .map(|frame| {
                let length = u32::from_le_bytes(frame[..4].try_into().unwrap()) as usize;
                assert_eq!(length, frame.len() - 4);
                assert!(frame.len() < 1024 * 1024, "{} bytes", frame.len());
                serde_json::from_slice(&frame[4..]).unwrap()
            })
            .collect()
    }

    fn join(frames: &[Value]) -> Value {
        let mut bytes = Vec::new();
        for (index, frame) in frames.iter().enumerate() {
            assert_eq!(frame["index"], index);
            assert_eq!(frame["count"], frames.len());
            let part = STANDARD.decode(frame["data"].as_str().unwrap()).unwrap();
            assert!(part.len() <= CHUNK_BYTES);
            bytes.extend(part);
        }
        serde_json::from_slice(&bytes).unwrap()
    }

    /// A message whose JSON is at least `size` bytes: exactly, for a
    /// one-byte `fill`.
    fn sized(size: usize, fill: &str) -> Value {
        let empty = serde_json::to_vec(&json!({"text": ""})).unwrap().len();
        let encoded = serde_json::to_string(fill).unwrap().len() - 2;
        let message = json!({"text": fill.repeat((size - empty).div_ceil(encoded))});
        assert!(serde_json::to_vec(&message).unwrap().len() >= size);
        message
    }

    #[test]
    fn small_messages_are_one_frame_and_large_ones_split_at_the_byte_limit() {
        let small = json!({"type":"servers","servers":[]});
        assert_eq!(frames_of(&small), std::slice::from_ref(&small));

        let limit = sized(CHUNK_BYTES, "a");
        assert_eq!(serde_json::to_vec(&limit).unwrap().len(), CHUNK_BYTES);
        assert_eq!(frames_of(&limit), std::slice::from_ref(&limit));

        let over = sized(CHUNK_BYTES + 1, "a");
        let frames = frames_of(&over);
        assert_eq!(frames.len(), 2);
        assert!(frames.iter().all(|frame| frame["type"] == "chunk"));
        assert_eq!(join(&frames), over);

        let big = sized(3 * CHUNK_BYTES + 17, "b");
        let frames = frames_of(&big);
        assert_eq!(frames.len(), 4);
        assert_eq!(join(&frames), big);
    }

    #[test]
    fn chunks_may_split_a_character_or_an_escape_and_still_join() {
        // Multibyte characters and JSON escapes straddle every boundary.
        for fill in ["é", "日本", "😀", "\"\\\n", "\u{1}"] {
            let message = sized(2 * CHUNK_BYTES + 3, fill);
            let frames = frames_of(&message);
            assert!(frames.len() >= 3);
            assert_eq!(join(&frames), message, "{fill:?}");
        }
    }

    #[tokio::test]
    async fn frames_round_trip_and_the_inbound_cap_holds() {
        let (mut writer, mut reader) = tokio::io::duplex(4 * 1024 * 1024);
        let message = json!({"type":"hello","protocol":1});
        for frame in encode(&message) {
            writer.write_all(&frame).await.unwrap();
        }
        drop(writer);
        let frame = read_frame(&mut reader).await.unwrap().unwrap();
        assert_eq!(serde_json::from_slice::<Value>(&frame).unwrap(), message);
        assert!(read_frame(&mut reader).await.unwrap().is_none());

        // A header announcing more than 64 MiB is refused before reading it.
        let (mut writer, mut reader) = tokio::io::duplex(64);
        let length = u32::try_from(MAX_INBOUND + 1).unwrap();
        writer.write_all(&length.to_le_bytes()).await.unwrap();
        let error = read_frame(&mut reader).await.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        // Exactly 64 MiB is allowed.
        let (mut writer, mut reader) = tokio::io::duplex(1024 * 1024);
        let length = u32::try_from(MAX_INBOUND).unwrap();
        let send = tokio::spawn(async move {
            writer.write_all(&length.to_le_bytes()).await.unwrap();
            writer.write_all(&vec![b' '; MAX_INBOUND]).await.unwrap();
        });
        assert_eq!(
            read_frame(&mut reader).await.unwrap().unwrap().len(),
            MAX_INBOUND
        );
        send.await.unwrap();
        // A stream cut inside a header is an error, not a clean end.
        let (mut writer, mut reader) = tokio::io::duplex(64);
        writer.write_all(&[1, 0]).await.unwrap();
        drop(writer);
        assert!(read_frame(&mut reader).await.is_err());
    }

    #[tokio::test]
    async fn one_writer_keeps_each_chunked_message_contiguous() {
        let (sender, receiver) = mpsc::unbounded_channel();
        let (writer, mut reader) = tokio::io::duplex(256 * 1024);
        let writing = tokio::spawn(write_messages(writer, receiver));
        let mut producers = Vec::new();
        for producer in 0..4 {
            let sender = sender.clone();
            producers.push(tokio::spawn(async move {
                for index in 0..3 {
                    let size = if index == 1 { 2 * CHUNK_BYTES } else { 64 };
                    let mut message = sized(size, "x");
                    message["producer"] = json!(producer);
                    message["index"] = json!(index);
                    sender.send(message).unwrap();
                    tokio::task::yield_now().await;
                }
            }));
        }
        drop(sender);
        let mut seen = Vec::new();
        while let Some(message) = read_message(&mut reader).await {
            seen.push((message["producer"].clone(), message["index"].clone()));
        }
        for producer in producers {
            producer.await.unwrap();
        }
        writing.await.unwrap().unwrap();
        assert_eq!(seen.len(), 12);
        for producer in 0..4 {
            let order: Vec<_> = seen
                .iter()
                .filter(|(from, _)| *from == producer)
                .map(|(_, index)| index.as_u64().unwrap())
                .collect();
            assert_eq!(order, [0, 1, 2], "producer {producer}");
        }
    }
}

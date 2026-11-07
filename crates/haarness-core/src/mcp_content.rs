//! What a model is given for data an MCP server hands back as a base64 blob:
//! an embedded resource in a tool's result, or the contents of a resource it
//! reads. Text comes back as text, images and PDFs as attachments within
//! [`MAX_ATTACHMENT_BYTES`], and anything else as a one-line note that says
//! what it is and how large, never as base64 text, which a model cannot read
//! and which costs about 1.4 characters a byte of its context.

use base64::{Engine as _, engine::general_purpose::STANDARD};

/// The decoded size of the images and PDFs one result attaches: a tool's
/// media, or what one resource read hands the model.
pub const MAX_ATTACHMENT_BYTES: usize = 10 * 1024 * 1024;

/// The most of one blob read as text. What is read then goes the way any
/// tool output does: cut for the model, with the whole saved to a file.
pub const MAX_TEXT_BYTES: usize = 10 * 1024 * 1024;

/// The images and documents a blob is attached as, as `read` attaches them.
const ATTACHABLE: [&str; 5] = [
    "image/png",
    "image/jpeg",
    "image/gif",
    "image/webp",
    "application/pdf",
];

/// `application/` types that hold text, besides those named `+json`,
/// `+xml` or `+yaml`.
const TEXT_APPLICATIONS: [&str; 15] = [
    "json",
    "xml",
    "yaml",
    "x-yaml",
    "toml",
    "csv",
    "sql",
    "graphql",
    "javascript",
    "x-javascript",
    "ecmascript",
    "x-ndjson",
    "jsonl",
    "x-sh",
    "x-shellscript",
];

/// What a model is given for one blob.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Blob {
    /// The blob's text, decoded.
    Text(String),
    /// An image or PDF to attach as `data_url`, and the line that stands
    /// for it in the text.
    Attached { data_url: String, note: String },
    /// A line that says what the blob is, standing in for what is not
    /// passed on.
    Note(String),
}

/// A media type as it is compared: what comes before any parameter, in
/// lower case, so `IMAGE/PNG; charset=binary` is `image/png`. An empty one
/// is `application/octet-stream`, as a blob without a type is.
#[must_use]
pub fn media_type(mime: &str) -> String {
    let bare = mime.split(';').next().unwrap_or_default().trim();
    if bare.is_empty() {
        "application/octet-stream".into()
    } else {
        bare.to_ascii_lowercase()
    }
}

/// Whether a [`media_type`] holds text a model reads as text: `text/*`,
/// JSON, XML, YAML, CSV and the like, and any type named `+json`, `+xml`
/// (an SVG) or `+yaml`.
#[must_use]
pub fn textual(media_type: &str) -> bool {
    let Some((kind, subtype)) = media_type.split_once('/') else {
        return false;
    };
    kind == "text"
        || ["+json", "+xml", "+yaml"]
            .iter()
            .any(|suffix| subtype.ends_with(suffix))
        || (kind == "application" && TEXT_APPLICATIONS.contains(&subtype))
}

/// `data` without the ASCII whitespace base64 may be broken over lines
/// with (76 characters a line is common), which decoding refuses.
#[must_use]
pub fn base64_text(data: &str) -> std::borrow::Cow<'_, str> {
    if data.bytes().any(|byte| byte.is_ascii_whitespace()) {
        data.chars()
            .filter(|c| !c.is_ascii_whitespace())
            .collect::<String>()
            .into()
    } else {
        data.into()
    }
}

/// What a model is given for `blob`, the base64 data of resource `uri`
/// declared as `mime`. Images and PDFs are attached while `attached` (the
/// decoded bytes attached so far) stays within [`MAX_ATTACHMENT_BYTES`],
/// and counted into it when they are; one past it is named instead, so
/// smaller ones after it still fit. An image whose type is mislabelled
/// (`image/jpg`) is attached as what its bytes are.
#[must_use]
pub fn blob(uri: &str, mime: Option<&str>, blob: &str, attached: &mut usize) -> Blob {
    let blob = base64_text(blob);
    let mut mime = media_type(mime.unwrap_or_default());
    let text = textual(&mime);
    let image = !text && mime.starts_with("image/");
    // Measured before it is decoded: four base64 characters hold three bytes.
    let estimate = blob.trim_end_matches('=').len() / 4 * 3;
    if text && estimate > MAX_TEXT_BYTES {
        return Blob::Note(format!(
            "[{uri}: {mime}, about {estimate} bytes, left out: Haarness reads at most 10 MiB of one resource as text]"
        ));
    }
    if (image || ATTACHABLE.contains(&mime.as_str()))
        && attached.saturating_add(estimate) > MAX_ATTACHMENT_BYTES
    {
        return over_attachments(uri, &mime, estimate);
    }
    let Ok(bytes) = STANDARD.decode(blob.as_bytes()) else {
        return Blob::Note(format!("[{uri}: {mime} content that is not valid base64]"));
    };
    if text {
        let body = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&bytes);
        // Text in another encoding (UTF-16 is mostly NUL bytes) is not read
        // as UTF-8 into noise.
        if body.contains(&0) {
            return Blob::Note(format!(
                "[{uri}: {mime}, {} bytes that are not UTF-8 text]",
                bytes.len()
            ));
        }
        return Blob::Text(String::from_utf8_lossy(body).into_owned());
    }
    if image
        && !ATTACHABLE.contains(&mime.as_str())
        && let Some(sniffed) =
            crate::images::sniff(&bytes).filter(|sniffed| ATTACHABLE.contains(sniffed))
    {
        sniffed.clone_into(&mut mime);
    }
    if ATTACHABLE.contains(&mime.as_str()) && !bytes.is_empty() {
        if attached.saturating_add(bytes.len()) > MAX_ATTACHMENT_BYTES {
            return over_attachments(uri, &mime, bytes.len());
        }
        *attached += bytes.len();
        return Blob::Attached {
            data_url: format!("data:{mime};base64,{blob}"),
            note: format!("[{mime} attachment: {uri}]"),
        };
    }
    Blob::Note(format!("[{uri}: {mime}, {} bytes]", bytes.len()))
}

/// A resource's image or PDF left out because the images and PDFs
/// attached with it (a tool's result, a read, or a message's mentions)
/// already come to [`MAX_ATTACHMENT_BYTES`].
fn over_attachments(uri: &str, mime: &str, bytes: usize) -> Blob {
    Blob::Note(format!(
        "[{uri}: {mime}, {bytes} bytes, left out: the images and PDFs attached with it already come to 10 MiB]"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encoded(bytes: &[u8]) -> String {
        STANDARD.encode(bytes)
    }

    /// Base64 broken over lines read as not valid base64, and an image
    /// labelled `image/jpg` was named rather than attached.
    #[test]
    fn wrapped_base64_and_a_mislabelled_image_are_read_as_what_they_are() {
        let jpeg = encoded(b"\xff\xd8\xff\xe0\x00\x10JFIF\x00fixture-jpeg-bytes-to-wrap");
        let wrapped = format!("{}\r\n{}\n", &jpeg[..20], &jpeg[20..]);
        let mut attached = 0;
        assert_eq!(
            blob("ui://photo", Some("image/jpg"), &wrapped, &mut attached),
            Blob::Attached {
                data_url: format!("data:image/jpeg;base64,{jpeg}"),
                note: "[image/jpeg attachment: ui://photo]".into()
            }
        );
        assert_eq!(
            blob(
                "notes://a",
                Some("text/plain"),
                &format!("{}\n", encoded(b"hello")),
                &mut attached
            ),
            Blob::Text("hello".into())
        );
        // An image type whose bytes are no image Haarness attaches is named.
        assert_eq!(
            blob(
                "ui://raw",
                Some("image/x-raw"),
                &encoded(&[7; 9]),
                &mut attached
            ),
            Blob::Note("[ui://raw: image/x-raw, 9 bytes]".into())
        );
    }

    #[test]
    fn media_types_compare_without_their_case_or_parameters() {
        assert_eq!(media_type("IMAGE/PNG; charset=binary"), "image/png");
        assert_eq!(media_type(" application/PDF "), "application/pdf");
        assert_eq!(media_type(""), "application/octet-stream");
        for text in [
            "text/plain",
            "text/markdown",
            "application/json",
            "application/vnd.api+json",
            "application/ld+json",
            "image/svg+xml",
            "application/x-yaml",
            "application/toml",
        ] {
            assert!(textual(text), "{text}");
        }
        for binary in [
            "image/png",
            "application/pdf",
            "application/zip",
            "application/octet-stream",
            "audio/mpeg",
            "json",
        ] {
            assert!(!textual(binary), "{binary}");
        }
    }

    #[test]
    fn text_blobs_come_back_as_their_text() {
        let mut attached = 0;
        assert_eq!(
            blob(
                "notes://a",
                Some("text/plain; charset=utf-8"),
                &encoded(b"\xef\xbb\xbfhello\nthere"),
                &mut attached
            ),
            Blob::Text("hello\nthere".into())
        );
        assert_eq!(
            blob(
                "data://b",
                Some("APPLICATION/JSON"),
                &encoded(br#"{"a":1}"#),
                &mut attached
            ),
            Blob::Text(r#"{"a":1}"#.into())
        );
        assert_eq!(
            blob(
                "notes://utf16",
                Some("text/plain; charset=utf-16le"),
                &encoded(b"h\0i\0"),
                &mut attached
            ),
            Blob::Note("[notes://utf16: text/plain, 4 bytes that are not UTF-8 text]".into())
        );
        let long = "x".repeat(MAX_TEXT_BYTES + 4);
        let Blob::Note(note) = blob(
            "notes://long",
            Some("text/plain"),
            &encoded(long.as_bytes()),
            &mut attached,
        ) else {
            panic!("a text past the limit was read");
        };
        assert!(
            note.starts_with("[notes://long: text/plain, about "),
            "{note}"
        );
        assert!(
            note.ends_with("left out: Haarness reads at most 10 MiB of one resource as text]"),
            "{note}"
        );
        assert_eq!(attached, 0);
    }

    #[test]
    fn images_attach_within_the_limit_and_other_bytes_are_named() {
        let mut attached = 0;
        let png = encoded(b"\x89PNG\r\n\x1a\nfixture");
        assert_eq!(
            blob(
                "ui://chart",
                Some("IMAGE/PNG; charset=binary"),
                &png,
                &mut attached
            ),
            Blob::Attached {
                data_url: format!("data:image/png;base64,{png}"),
                note: "[image/png attachment: ui://chart]".into()
            }
        );
        assert_eq!(attached, 15);
        assert_eq!(
            blob(
                "files://a.zip",
                Some("application/zip"),
                &encoded(&[0; 300]),
                &mut attached
            ),
            Blob::Note("[files://a.zip: application/zip, 300 bytes]".into())
        );
        assert_eq!(
            blob("files://b", None, &encoded(&[1, 2, 3]), &mut attached),
            Blob::Note("[files://b: application/octet-stream, 3 bytes]".into())
        );
        assert_eq!(
            blob("files://c", Some("image/png"), "not base64!", &mut attached),
            Blob::Note("[files://c: image/png content that is not valid base64]".into())
        );
        // One past the limit is named, and a smaller one after it still fits.
        let big = encoded(&vec![0; MAX_ATTACHMENT_BYTES]);
        let Blob::Note(note) = blob(
            "files://big.pdf",
            Some("application/pdf"),
            &big,
            &mut attached,
        ) else {
            panic!("an attachment past the limit was attached");
        };
        assert!(
            note.ends_with(
                "left out: the images and PDFs attached with it already come to 10 MiB]"
            ),
            "{note}"
        );
        assert!(matches!(
            blob("ui://small", Some("image/gif"), &png, &mut attached),
            Blob::Attached { .. }
        ));
        assert_eq!(attached, 30);
    }
}

//! What a person may attach to a message, and how attachments reach a model.
//!
//! Providers take images, PDFs, audio and video. A text file (notes, code,
//! JSON) is read into the conversation as text instead, so a client may
//! attach any file a person would share. Anything else is refused before the
//! message is stored: a stored attachment the provider rejects would fail
//! every later turn of that chat.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use base64::{Engine as _, engine::general_purpose::STANDARD};

use crate::{CoreError, CoreResult};

/// The largest text file read into a message.
pub const MAX_TEXT_ATTACHMENT_BYTES: usize = 512 * 1024;

/// The media type a provider receives for a file, by its extension.
pub fn media_mime_for_path(path: &Path) -> Option<&'static str> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    match extension.as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "webp" => Some("image/webp"),
        "gif" => Some("image/gif"),
        "heic" => Some("image/heic"),
        "heif" => Some("image/heif"),
        "pdf" => Some("application/pdf"),
        "mp3" => Some("audio/mpeg"),
        "wav" => Some("audio/wav"),
        "flac" => Some("audio/flac"),
        "m4a" => Some("audio/mp4"),
        "aac" => Some("audio/aac"),
        "ogg" | "oga" | "opus" => Some("audio/ogg"),
        "mp4" | "m4v" => Some("video/mp4"),
        "mpeg" => Some("video/mpeg"),
        "mov" => Some("video/mov"),
        "avi" => Some("video/avi"),
        "flv" => Some("video/x-flv"),
        "mpg" => Some("video/mpg"),
        "webm" => Some("video/webm"),
        "wmv" => Some("video/wmv"),
        "3gp" | "3gpp" => Some("video/3gpp"),
        _ => None,
    }
}

/// Media types providers accept inline.
pub fn is_supported_media_mime(mime_type: &str) -> bool {
    matches!(
        mime_type,
        "image/png"
            | "image/jpeg"
            | "image/webp"
            | "image/gif"
            | "image/heic"
            | "image/heif"
            | "application/pdf"
            | "audio/mpeg"
            | "audio/wav"
            | "audio/flac"
            | "audio/mp4"
            | "audio/aac"
            | "audio/ogg"
            | "video/mp4"
            | "video/mpeg"
            | "video/mov"
            | "video/avi"
            | "video/x-flv"
            | "video/mpg"
            | "video/webm"
            | "video/wmv"
            | "video/3gpp"
    )
}

/// Checks a new message's attachments. Media stays as sent; a text file named
/// by path is carried as a text data URL, so the message keeps it even if the
/// file changes; anything a model cannot read is refused.
pub fn admit(attachments: Vec<String>, workspace: &Path) -> CoreResult<Vec<String>> {
    attachments
        .into_iter()
        .map(|source| admit_one(source, workspace))
        .collect()
}

fn admit_one(source: String, workspace: &Path) -> CoreResult<String> {
    if let Some(data) = source.strip_prefix("data:") {
        let url = DataUrl::parse(data)?;
        if is_supported_media_mime(url.mime_type) {
            return Ok(source);
        }
        let bytes = url.bytes()?;
        if text(&bytes, url.mime_type).is_none() {
            return Err(unsupported(&url.name()));
        }
        if bytes.len() > MAX_TEXT_ATTACHMENT_BYTES {
            return Err(too_large(&url.name()));
        }
        return Ok(source);
    }
    if source.starts_with("http://") || source.starts_with("https://") {
        return Err(CoreError::Invalid(
            "remote attachment URLs are not fetched; attach a local file or data URL".into(),
        ));
    }
    let path = local_path(&source)
        .ok_or_else(|| CoreError::Invalid(format!("invalid file URL attachment: {source}")))?;
    // A relative path names a file in the workspace; storing it absolute lets
    // the model read it whatever directory the server runs from.
    let relative = path.is_relative();
    let path = if relative { workspace.join(path) } else { path };
    let metadata = std::fs::metadata(&path)
        .ok()
        .filter(std::fs::Metadata::is_file)
        .ok_or_else(|| CoreError::Invalid(format!("attachment not found: {source}")))?;
    if media_mime_for_path(&path).is_some() {
        return Ok(if relative {
            path.to_string_lossy().into_owned()
        } else {
            source
        });
    }
    let name = path.file_name().map_or_else(
        || "attachment".into(),
        |name| name.to_string_lossy().into_owned(),
    );
    if usize::try_from(metadata.len()).unwrap_or(usize::MAX) > MAX_TEXT_ATTACHMENT_BYTES {
        return Err(too_large(&name));
    }
    let bytes = std::fs::read(&path)
        .map_err(|error| CoreError::Invalid(format!("cannot read attachment {name}: {error}")))?;
    if text(&bytes, "").is_none() {
        return Err(unsupported(&name));
    }
    Ok(format!(
        "data:text/plain;name={};base64,{}",
        url::form_urlencoded::byte_serialize(name.as_bytes()).collect::<String>(),
        STANDARD.encode(bytes)
    ))
}

/// A stored message as a model receives it: text files are appended to the
/// message, and media that can no longer be read (an upload folder deleted
/// since) is named instead of failing the turn.
pub fn for_model(content: String, attachments: Vec<String>) -> (String, Vec<String>) {
    let mut content = content;
    let mut media = Vec::new();
    for source in attachments {
        if let Some(data) = source.strip_prefix("data:") {
            match DataUrl::parse(data) {
                Ok(url) if is_supported_media_mime(url.mime_type) => media.push(source),
                Ok(url) => {
                    let name = url.name();
                    match url
                        .bytes()
                        .ok()
                        .and_then(|bytes| text(&bytes, url.mime_type))
                    {
                        Some(body) => {
                            let _ = write!(
                                content,
                                "\n\n<attached-file name=\"{name}\">\n{}\n</attached-file>",
                                body.trim_end()
                            );
                        }
                        None => {
                            let _ = write!(content, "\n[Attachment {name} cannot be read]");
                        }
                    }
                }
                Err(_) => content.push_str("\n[An attachment cannot be read]"),
            }
            continue;
        }
        let readable = local_path(&source).is_some_and(|path| path.is_file());
        if readable || source.starts_with("http://") || source.starts_with("https://") {
            media.push(source);
        } else {
            let _ = write!(content, "\n[Attachment {source} is no longer available]");
        }
    }
    (content, media)
}

struct DataUrl<'a> {
    mime_type: &'a str,
    parameters: Vec<&'a str>,
    encoded: &'a str,
}

impl<'a> DataUrl<'a> {
    fn parse(data: &'a str) -> CoreResult<Self> {
        let (metadata, encoded) = data
            .split_once(',')
            .ok_or_else(|| CoreError::Invalid("invalid attachment data URL".into()))?;
        let mut metadata = metadata.split(';');
        let mime_type = metadata.next().unwrap_or_default();
        let parameters = metadata.collect::<Vec<_>>();
        if !parameters.contains(&"base64") {
            return Err(CoreError::Invalid(
                "attachment data URL must use base64".into(),
            ));
        }
        Ok(Self {
            mime_type,
            parameters,
            encoded,
        })
    }

    fn bytes(&self) -> CoreResult<Vec<u8>> {
        STANDARD
            .decode(self.encoded)
            .map_err(|error| CoreError::Invalid(format!("invalid attachment base64: {error}")))
    }

    fn name(&self) -> String {
        self.parameters
            .iter()
            .find_map(|part| part.strip_prefix("name="))
            .and_then(|name| url::form_urlencoded::parse(name.as_bytes()).next())
            .map_or_else(|| self.mime_type.to_owned(), |(name, _)| name.into_owned())
    }
}

/// The file as text, when it is text: a text type, or an untyped file that
/// decodes as UTF-8. Binary files (archives, executables) are not.
fn text(bytes: &[u8], mime_type: &str) -> Option<String> {
    let essence = mime_type.trim().to_ascii_lowercase();
    let textual = essence.is_empty()
        || essence.starts_with("text/")
        || essence.ends_with("+json")
        || essence.ends_with("+xml")
        || matches!(
            essence.as_str(),
            "application/octet-stream"
                | "application/json"
                | "application/xml"
                | "application/yaml"
                | "application/x-yaml"
                | "application/toml"
                | "application/javascript"
                | "application/x-javascript"
                | "application/typescript"
                | "application/x-sh"
                | "application/x-shellscript"
                | "application/sql"
                | "application/graphql"
                | "application/x-ndjson"
        );
    if !textual {
        return None;
    }
    let text = std::str::from_utf8(bytes).ok()?;
    (!text.contains('\0')).then(|| text.to_owned())
}

fn local_path(source: &str) -> Option<PathBuf> {
    match source.strip_prefix("file://") {
        Some(_) => url::Url::parse(source).ok()?.to_file_path().ok(),
        None => Some(PathBuf::from(source)),
    }
}

fn unsupported(name: &str) -> CoreError {
    CoreError::Invalid(format!(
        "{name} cannot be attached: attach an image, PDF, audio, video or text file"
    ))
}

fn too_large(name: &str) -> CoreError {
    CoreError::Invalid(format!(
        "{name} is larger than the {} KiB text attachment limit",
        MAX_TEXT_ATTACHMENT_BYTES / 1024
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data_url(mime: &str, name: Option<&str>, bytes: &[u8]) -> String {
        let name = name.map(|name| format!(";name={name}")).unwrap_or_default();
        format!("data:{mime}{name};base64,{}", STANDARD.encode(bytes))
    }

    #[test]
    fn media_is_kept_as_sent() {
        let png = data_url("image/png", None, b"\x89PNG");
        assert_eq!(admit(vec![png.clone()], Path::new("/")).unwrap(), vec![png]);
    }

    #[test]
    fn text_files_are_admitted_and_reach_the_model_as_text() {
        let notes = data_url("text/plain", Some("notes.txt"), b"code word PELICAN\n");
        let json = data_url("application/json", Some("a.json"), b"{\"a\":1}");
        let untyped = data_url("application/octet-stream", Some("Makefile"), b"all:\n");
        let admitted = admit(vec![notes, json, untyped], Path::new("/")).unwrap();
        let (content, media) = for_model("Read these".into(), admitted);
        assert!(media.is_empty());
        assert!(content.starts_with("Read these"));
        assert!(
            content.contains(
                "<attached-file name=\"notes.txt\">\ncode word PELICAN\n</attached-file>"
            )
        );
        assert!(content.contains("{\"a\":1}"));
        assert!(content.contains("<attached-file name=\"Makefile\">"));
    }

    #[test]
    fn binary_and_unknown_files_are_refused_before_they_are_stored() {
        for source in [
            data_url("application/zip", Some("a.zip"), b"PK\x03\x04"),
            data_url("application/octet-stream", Some("a.bin"), b"\0\x01\x02"),
            data_url(
                "application/vnd.ms-excel",
                Some("a.xls"),
                b"\xd0\xcf\x11\xe0",
            ),
        ] {
            let error = admit(vec![source], Path::new("/")).unwrap_err();
            assert!(error.to_string().contains("cannot be attached"), "{error}");
        }
        assert!(admit(vec!["data:text/plain,hello".into()], Path::new("/")).is_err());
    }

    #[test]
    fn text_files_by_path_travel_with_the_message() {
        let dir = std::env::temp_dir().join(format!("haarness-attach-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("plan.md"), "# Plan\n").unwrap();
        std::fs::write(dir.join("shot.png"), b"\x89PNG").unwrap();
        let admitted = admit(vec!["plan.md".into(), "shot.png".into()], &dir).unwrap();
        assert!(admitted[0].starts_with("data:text/plain;name=plan.md;base64,"));
        assert_eq!(admitted[1], dir.join("shot.png").to_string_lossy());
        let missing = admit(vec!["gone.png".into()], &dir).unwrap_err();
        assert!(missing.to_string().contains("not found"), "{missing}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn media_deleted_since_is_named_instead_of_failing() {
        let (content, media) = for_model(
            "Look".into(),
            vec![
                "/nonexistent/upload/shot.png".into(),
                data_url("image/png", None, b"x"),
            ],
        );
        assert_eq!(media.len(), 1);
        assert!(
            content.contains("[Attachment /nonexistent/upload/shot.png is no longer available]")
        );
    }

    #[test]
    fn text_over_the_limit_is_refused() {
        let big = data_url(
            "text/plain",
            Some("big.log"),
            &vec![b'a'; MAX_TEXT_ATTACHMENT_BYTES + 1],
        );
        assert!(
            admit(vec![big], Path::new("/"))
                .unwrap_err()
                .to_string()
                .contains("limit")
        );
    }
}

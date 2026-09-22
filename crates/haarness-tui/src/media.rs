//! Terminal paste/drop recognition for local document and media attachments.

use std::path::{Path, PathBuf};

use base64::{Engine as _, engine::general_purpose::STANDARD};

const MAX_ATTACHMENTS: usize = 8;
const MAX_INLINE_BYTES: u64 = 20 * 1024 * 1024;

/// Turn client-local paths into data URLs for an explicitly attached server.
/// The filename parameter is display-only; provider adapters ignore it while
/// retaining the MIME type and payload.
pub async fn inline_sources(sources: Vec<String>) -> anyhow::Result<Vec<String>> {
    let mut total = 0_u64;
    let mut result = Vec::with_capacity(sources.len());
    for source in sources {
        if source.starts_with("data:")
            || source.starts_with("http://")
            || source.starts_with("https://")
        {
            result.push(source);
            continue;
        }
        let path = if source.starts_with("file://") {
            url::Url::parse(&source)
                .ok()
                .and_then(|url| url.to_file_path().ok())
                .ok_or_else(|| anyhow::anyhow!("invalid file URL attachment: {source}"))?
        } else {
            PathBuf::from(&source)
        };
        let mime = mime_for_path(&path)
            .ok_or_else(|| anyhow::anyhow!("unsupported media attachment {}", path.display()))?;
        let bytes = tokio::fs::read(&path).await.map_err(|error| {
            anyhow::anyhow!("cannot read attachment {}: {error}", path.display())
        })?;
        total = total.saturating_add(bytes.len() as u64);
        if total > MAX_INLINE_BYTES {
            anyhow::bail!("attachments exceed the 20 MiB inline media limit");
        }
        let name = path.file_name().map_or_else(
            || "attachment".into(),
            |name| name.to_string_lossy().into_owned(),
        );
        let name = encode_name(&name);
        result.push(format!(
            "data:{mime};name={name};base64,{}",
            STANDARD.encode(bytes)
        ));
    }
    Ok(result)
}

fn encode_name(value: &str) -> String {
    value
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
                (byte as char).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect()
}

/// Compact transcript identity for both local paths and uploaded data URLs.
pub fn attachment_display(source: &str) -> (&str, String) {
    if let Some(metadata) = source
        .strip_prefix("data:")
        .and_then(|data| data.split_once(','))
        .map(|value| value.0)
    {
        let mut parts = metadata.split(';');
        let mime = parts.next().unwrap_or_default();
        let name = parts
            .find_map(|part| part.strip_prefix("name="))
            .and_then(|name| {
                url::form_urlencoded::parse(name.as_bytes())
                    .next()
                    .map(|value| value.0.into_owned())
            })
            .unwrap_or_else(|| "uploaded attachment".into());
        return (badge_for_mime(mime), name);
    }
    let path = Path::new(source);
    let badge = mime_for_path(path).map_or("file", badge_for_mime);
    let name = path.file_name().map_or_else(
        || source.to_owned(),
        |name| name.to_string_lossy().into_owned(),
    );
    (badge, name)
}

/// Recognize a bracketed paste as one or more local attachment paths. `None`
/// means ordinary text; `Err` means it was definitely a file drop but could
/// not be accepted and should be reported rather than inserted as a prompt.
pub fn paths_from_paste(value: &str) -> Result<Option<Vec<String>>, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let whole = decode_path(trimmed);
    let candidates = if whole.as_ref().is_some_and(|path| path.is_file()) {
        vec![whole.unwrap_or_default()]
    } else {
        let tokens = shellish_words(trimmed);
        if tokens.is_empty() {
            return Ok(None);
        }
        let paths: Vec<_> = tokens
            .iter()
            .filter_map(|token| decode_path(token))
            .collect();
        if paths.len() != tokens.len() || paths.iter().any(|path| !path.is_file()) {
            return Ok(None);
        }
        paths
    };
    if candidates.len() > MAX_ATTACHMENTS {
        return Err(format!("Attach at most {MAX_ATTACHMENTS} files at once"));
    }
    let mut total = 0_u64;
    let mut result = Vec::new();
    for path in candidates {
        if mime_for_path(&path).is_none() {
            return Err(format!(
                "{} is not a supported image, PDF, audio, or video",
                path.file_name().map_or_else(
                    || path.display().to_string(),
                    |name| name.to_string_lossy().into()
                )
            ));
        }
        let size = path
            .metadata()
            .map_err(|error| format!("Cannot read {}: {error}", path.display()))?
            .len();
        total = total.saturating_add(size);
        if total > MAX_INLINE_BYTES {
            return Err("Attachments exceed the 20 MiB inline media limit".into());
        }
        let canonical = path
            .canonicalize()
            .map_err(|error| format!("Cannot resolve {}: {error}", path.display()))?;
        result.push(canonical.display().to_string());
    }
    Ok(Some(result))
}

pub fn mime_for_path(path: &Path) -> Option<&'static str> {
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

pub fn badge_for_mime(mime: &str) -> &'static str {
    if mime == "application/pdf" {
        "pdf"
    } else if mime.starts_with("audio/") {
        "aud"
    } else if mime.starts_with("video/") {
        "vid"
    } else if mime.starts_with("image/") {
        "img"
    } else {
        "file"
    }
}

fn decode_path(value: &str) -> Option<PathBuf> {
    let unquoted = value
        .strip_prefix('\'')
        .and_then(|value| value.strip_suffix('\''))
        .or_else(|| {
            value
                .strip_prefix('"')
                .and_then(|value| value.strip_suffix('"'))
        })
        .unwrap_or(value);
    if unquoted.starts_with("file://") {
        return url::Url::parse(unquoted).ok()?.to_file_path().ok();
    }
    Some(PathBuf::from(unescape(unquoted)))
}

/// Small shell-word reader for the forms terminal emulators emit when files
/// are dropped: quoted paths or spaces escaped with a backslash.
fn shellish_words(value: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut escaped = false;
    for character in value.chars() {
        if escaped {
            current.push(character);
            escaped = false;
            continue;
        }
        if character == '\\' && quote != Some('\'') {
            escaped = true;
            continue;
        }
        if matches!(character, '\'' | '"') {
            if quote == Some(character) {
                quote = None;
            } else if quote.is_none() {
                quote = Some(character);
            } else {
                current.push(character);
            }
            continue;
        }
        if character.is_whitespace() && quote.is_none() {
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
        } else {
            current.push(character);
        }
    }
    if escaped {
        current.push('\\');
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

fn unescape(value: &str) -> String {
    let mut result = String::with_capacity(value.len());
    let mut escaped = false;
    for character in value.chars() {
        if escaped {
            result.push(character);
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else {
            result.push(character);
        }
    }
    if escaped {
        result.push('\\');
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_absolute_text_is_not_mistaken_for_a_drop() {
        assert_eq!(
            paths_from_paste("/definitely/not/a/file.png").unwrap(),
            None
        );
    }

    #[test]
    fn shellish_words_handle_terminal_drop_escaping() {
        assert_eq!(
            shellish_words("/tmp/one\\ file.png '/tmp/two file.mp4'"),
            vec!["/tmp/one file.png", "/tmp/two file.mp4"]
        );
    }

    #[test]
    fn recognizes_a_real_dropped_image_path() {
        let root = std::env::temp_dir().join(format!("haarness-media-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&root).unwrap();
        let image = root.join("screen shot.png");
        std::fs::write(&image, b"png fixture").unwrap();
        let escaped = image.display().to_string().replace(' ', "\\ ");

        let paths = paths_from_paste(&escaped).unwrap().unwrap();
        assert_eq!(
            paths,
            vec![image.canonicalize().unwrap().display().to_string()]
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn explicit_attach_uploads_media_and_preserves_its_name() {
        let root = std::env::temp_dir().join(format!("haarness-upload-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&root).unwrap();
        let image = root.join("screen shot.png");
        std::fs::write(&image, b"png fixture").unwrap();

        let source = inline_sources(vec![image.display().to_string()])
            .await
            .unwrap()
            .remove(0);
        assert!(source.starts_with("data:image/png;name=screen%20shot.png;base64,"));
        assert_eq!(
            attachment_display(&source),
            ("img", "screen shot.png".into())
        );

        let file = std::fs::File::create(&image).unwrap();
        file.set_len(11 * 1024 * 1024).unwrap();
        assert!(paths_from_paste(image.to_str().unwrap()).unwrap().is_some());
        assert!(inline_sources(vec![image.display().to_string()]).await.is_ok());
        file.set_len(20 * 1024 * 1024 + 1).unwrap();
        assert!(paths_from_paste(image.to_str().unwrap()).is_err());
        assert!(inline_sources(vec![image.display().to_string()]).await.is_err());

        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn explicit_attach_uploads_pdf_and_audio_with_distinct_badges() {
        let root = std::env::temp_dir().join(format!("haarness-docs-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&root).unwrap();
        let pdf = root.join("design notes.pdf");
        let audio = root.join("meeting.mp3");
        std::fs::write(&pdf, b"%PDF-1.4\n%%EOF\n").unwrap();
        std::fs::write(&audio, b"ID3").unwrap();

        let sources = inline_sources(vec![pdf.display().to_string(), audio.display().to_string()])
            .await
            .unwrap();
        assert!(sources[0].starts_with("data:application/pdf;name=design%20notes.pdf;base64,"));
        assert!(sources[1].starts_with("data:audio/mpeg;name=meeting.mp3;base64,"));
        assert_eq!(
            attachment_display(&sources[0]),
            ("pdf", "design notes.pdf".into())
        );
        assert_eq!(
            attachment_display(&sources[1]),
            ("aud", "meeting.mp3".into())
        );

        let _ = std::fs::remove_dir_all(root);
    }
}

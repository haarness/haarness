//! Terminal paste/drop recognition for local document and media attachments.

use std::io::Read as _;
use std::path::{Path, PathBuf};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use tokio::io::AsyncReadExt as _;

const MAX_ATTACHMENTS: usize = 8;
const MAX_INLINE_BYTES: u64 = haarness_protocol::MAX_INLINE_MEDIA_BYTES as u64;
/// How far into a file PDF readers look for its `%PDF-` header.
const PDF_HEADER_WINDOW: u64 = 1024;

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
        // What is left of the limit, and one byte more: a file over it is
        // refused once that byte arrives, without reading the rest.
        let room = MAX_INLINE_BYTES.saturating_sub(total);
        let mut bytes = Vec::new();
        tokio::fs::File::open(&path)
            .await
            .map_err(|error| anyhow::anyhow!("cannot read attachment {}: {error}", path.display()))?
            .take(room.saturating_add(1))
            .read_to_end(&mut bytes)
            .await
            .map_err(|error| {
                anyhow::anyhow!("cannot read attachment {}: {error}", path.display())
            })?;
        total = total.saturating_add(bytes.len() as u64);
        if total > MAX_INLINE_BYTES {
            // The server's words: an attached server refuses no less.
            anyhow::bail!(over_limit(crate::locale::Language::English, total));
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

/// Why a message is refused for its media, as the server words it: the
/// total, in the interface language.
pub fn over_limit(language: crate::locale::Language, total: u64) -> String {
    language.format(
        "The attachments come to {size}, over the {limit} limit for media in one message",
        &[
            ("size", &haarness_protocol::mebibytes(total)),
            ("limit", &haarness_protocol::mebibytes(MAX_INLINE_BYTES)),
        ],
    )
}

/// Recognize a bracketed paste as one or more local media paths to attach
/// next to `attached` (the draft's attachments). `None` means ordinary text:
/// not paths, or a path to a file that is not media (a README, or one named
/// `.pdf` that is not a PDF), which a prompt names as text. `Err` means it
/// was media that cannot be accepted and should be reported, in `language`,
/// rather than inserted as a prompt.
pub fn paths_from_paste(
    value: &str,
    attached: &[String],
    language: crate::locale::Language,
) -> Result<Option<Vec<String>>, String> {
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
    if !candidates.iter().all(|path| is_media(path)) {
        return Ok(None);
    }
    if candidates.len() > MAX_ATTACHMENTS {
        return Err(format!("Attach at most {MAX_ATTACHMENTS} files at once"));
    }
    // Counted as the server counts the message, what is attached already
    // included. Images are left out: it shrinks each to a few MB, so only
    // other media can take a message past the limit here.
    let mut total = attached
        .iter()
        .filter(|source| !is_image(source))
        .map(|source| attached_bytes(source))
        .fold(0_u64, u64::saturating_add);
    let mut result: Vec<String> = Vec::new();
    for path in candidates {
        let size = path
            .metadata()
            .map_err(|error| format!("Cannot read {}: {error}", path.display()))?
            .len();
        let canonical = path
            .canonicalize()
            .map_err(|error| format!("Cannot resolve {}: {error}", path.display()))?
            .display()
            .to_string();
        let counted = attached.contains(&canonical) || result.contains(&canonical);
        if !counted && !is_image(&canonical) {
            total = total.saturating_add(size);
            if total > MAX_INLINE_BYTES {
                return Err(over_limit(language, total));
            }
        }
        result.push(canonical);
    }
    Ok(Some(result))
}

/// An image attachment, a file or a data URL, judged by its name or type.
fn is_image(source: &str) -> bool {
    source.strip_prefix("data:").map_or_else(
        || mime_for_path(Path::new(source)).is_some_and(|mime| mime.starts_with("image/")),
        |data| data.starts_with("image/"),
    )
}

/// Whether a file is media a message attaches: an image, audio or video by
/// its name, or a PDF whose first kilobyte holds the `%PDF-` header (a Git
/// LFS pointer named `.pdf` is text).
fn is_media(path: &Path) -> bool {
    match mime_for_path(path) {
        Some("application/pdf") => {
            let mut head = Vec::new();
            std::fs::File::open(path)
                .and_then(|file| file.take(PDF_HEADER_WINDOW).read_to_end(&mut head))
                .is_ok_and(|_| head.windows(5).any(|part| part == b"%PDF-"))
        }
        Some(_) => true,
        None => false,
    }
}

/// The bytes an attachment of the draft sends: a data URL's payload, or a
/// file's length (nothing for one that is gone).
fn attached_bytes(source: &str) -> u64 {
    if let Some((_, encoded)) = source
        .strip_prefix("data:")
        .and_then(|data| data.split_once(','))
    {
        let padding = encoded
            .bytes()
            .rev()
            .take_while(|&byte| byte == b'=')
            .count();
        return (encoded.len() / 4 * 3).saturating_sub(padding) as u64;
    }
    std::fs::metadata(source).map_or(0, |metadata| metadata.len())
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
        // The server converts these, and HEIC, to PNG or JPEG.
        "bmp" => Some("image/bmp"),
        "tif" | "tiff" => Some("image/tiff"),
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
    use crate::locale::Language;

    #[test]
    fn ordinary_absolute_text_is_not_mistaken_for_a_drop() {
        assert_eq!(
            paths_from_paste("/definitely/not/a/file.png", &[], Language::English).unwrap(),
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

        let paths = paths_from_paste(&escaped, &[], Language::English)
            .unwrap()
            .unwrap();
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
        assert!(
            paths_from_paste(image.to_str().unwrap(), &[], Language::English)
                .unwrap()
                .is_some()
        );
        assert!(
            inline_sources(vec![image.display().to_string()])
                .await
                .is_ok()
        );
        // The server shrinks an image, so a paste takes a large one; an
        // upload to an attached server still carries it whole.
        file.set_len(20 * 1024 * 1024 + 1).unwrap();
        assert!(
            paths_from_paste(image.to_str().unwrap(), &[], Language::English)
                .unwrap()
                .is_some()
        );
        let error = inline_sources(vec![image.display().to_string()])
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "The attachments come to 20.1 MiB, over the 20 MiB limit for media in one message"
        );

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

    #[test]
    fn a_pasted_path_to_a_file_that_is_not_media_stays_text() {
        let root = std::env::temp_dir().join(format!("haarness-paste-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&root).unwrap();
        let readme = root.join("README.md");
        std::fs::write(&readme, "# Notes\n").unwrap();
        let image = root.join("shot.png");
        std::fs::write(&image, b"png fixture").unwrap();

        assert_eq!(
            paths_from_paste(readme.to_str().unwrap(), &[], Language::English),
            Ok(None)
        );
        // Together with media too: the paste is text naming both.
        let both = format!("{} {}", image.display(), readme.display());
        assert_eq!(paths_from_paste(&both, &[], Language::English), Ok(None));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_file_named_pdf_counts_as_one_only_when_it_holds_one() {
        let root = std::env::temp_dir().join(format!("haarness-paste-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&root).unwrap();
        let pointer = root.join("design.pdf");
        std::fs::write(
            &pointer,
            "version https://git-lfs.github.com/spec/v1\noid sha256:4d7a\nsize 1048576\n",
        )
        .unwrap();
        assert_eq!(
            paths_from_paste(pointer.to_str().unwrap(), &[], Language::English),
            Ok(None)
        );

        // A real one, its header after a little junk as readers allow.
        let real = root.join("report.pdf");
        std::fs::write(&real, b"\r\n%PDF-1.7\n%%EOF\n").unwrap();
        assert_eq!(
            paths_from_paste(real.to_str().unwrap(), &[], Language::English),
            Ok(Some(vec![
                real.canonicalize().unwrap().display().to_string()
            ]))
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn images_are_left_out_of_a_paste_total_since_the_server_shrinks_them() {
        let root = std::env::temp_dir().join(format!("haarness-paste-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&root).unwrap();
        let shots: Vec<_> = ["one.png", "two.png"]
            .iter()
            .map(|name| {
                let shot = root.join(name);
                std::fs::File::create(&shot)
                    .unwrap()
                    .set_len(12 * 1024 * 1024)
                    .unwrap();
                shot
            })
            .collect();
        let first = paths_from_paste(shots[0].to_str().unwrap(), &[], Language::English)
            .unwrap()
            .unwrap();
        assert!(
            paths_from_paste(shots[1].to_str().unwrap(), &first, Language::English)
                .unwrap()
                .is_some()
        );
        let both = format!("{} {}", shots[0].display(), shots[1].display());
        assert_eq!(
            paths_from_paste(&both, &[], Language::English)
                .unwrap()
                .map(|paths| paths.len()),
            Some(2)
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_paste_counts_what_the_draft_already_carries() {
        let root = std::env::temp_dir().join(format!("haarness-paste-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&root).unwrap();
        let first = root.join("first.mp4");
        let second = root.join("second.mp4");
        for file in [&first, &second] {
            std::fs::File::create(file)
                .unwrap()
                .set_len(12 * 1024 * 1024)
                .unwrap();
        }
        let attached = vec![first.canonicalize().unwrap().display().to_string()];
        assert!(
            paths_from_paste(second.to_str().unwrap(), &[], Language::English)
                .unwrap()
                .is_some()
        );
        assert_eq!(
            paths_from_paste(second.to_str().unwrap(), &attached, Language::English),
            Err(
                "The attachments come to 24 MiB, over the 20 MiB limit for media in one message"
                    .into()
            )
        );
        // In the interface language.
        assert_eq!(
            paths_from_paste(second.to_str().unwrap(), &attached, Language::German),
            Err(Language::German.format(
                "The attachments come to {size}, over the {limit} limit for media in one message",
                &[("size", "24 MiB"), ("limit", "20 MiB")]
            ))
        );
        // A file attached already is not counted twice.
        let again = format!("{} {}", first.display(), second.display());
        assert!(paths_from_paste(&again, &attached, Language::English).is_err());
        assert!(
            paths_from_paste(first.to_str().unwrap(), &attached, Language::English)
                .unwrap()
                .is_some()
        );
        // A data URL already attached (a picked server file) counts as well.
        let picked = format!(
            "data:application/pdf;base64,{}",
            STANDARD.encode(vec![0_u8; 12 * 1024 * 1024])
        );
        assert!(paths_from_paste(second.to_str().unwrap(), &[picked], Language::English).is_err());

        let _ = std::fs::remove_dir_all(root);
    }
}

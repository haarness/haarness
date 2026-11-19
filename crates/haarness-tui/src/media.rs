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
        let name = path.file_name().map_or_else(
            || "attachment".into(),
            |name| name.to_string_lossy().into_owned(),
        );
        // The server's words: an attached server refuses no less.
        let size = tokio::fs::metadata(&path)
            .await
            .map_or(0, |metadata| metadata.len());
        if size > MAX_INLINE_BYTES {
            anyhow::bail!(file_over_limit(
                crate::locale::Language::English,
                &name,
                size
            ));
        }
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
            anyhow::bail!(over_limit(crate::locale::Language::English, total));
        }
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

/// Why a message is refused for one file's media, as the server words it.
pub fn file_over_limit(language: crate::locale::Language, name: &str, size: u64) -> String {
    language.format(
        "{name} is {size}, over the {limit} limit for media in one message",
        &[
            ("name", name),
            ("size", &haarness_protocol::mebibytes(size)),
            ("limit", &haarness_protocol::mebibytes(MAX_INLINE_BYTES)),
        ],
    )
}

/// What a drop of files brings to the prompt: the media to attach, and the
/// other files' paths, which stay text.
#[derive(Debug, PartialEq, Eq)]
pub struct Dropped {
    /// Canonical paths of the media files, in the order they were dropped.
    pub media: Vec<String>,
    /// The other files as the terminal wrote them (escaped or quoted), one
    /// space apart; empty when every file is media.
    pub text: String,
}

/// Media a paste held that cannot be attached: why, as the server words it,
/// and whether the paste still goes into the prompt as text.
#[derive(Debug, PartialEq, Eq)]
pub struct Refusal {
    pub reason: String,
    /// A drop that named other files too is text as well, so none of it is
    /// lost; one of media alone is refused whole, as it always was.
    pub keep_text: bool,
}

/// Recognize a bracketed paste as a drop of local files that holds media to
/// attach next to `attached` (the draft's attachments). `None` means
/// ordinary text: not paths, or only files that are not media (a README, or
/// one named `.pdf` that is not a PDF), which a prompt names as text. A drop
/// of media and other files, on one line with every path absolute as
/// terminals write a drop, attaches the media and keeps the others' paths as
/// text; any other mix of files, such as a pasted list of the project's
/// files, is text. `Err` means it held media that cannot be accepted and
/// should be reported, in `language`. `uploads` says the files go to the
/// server whole (an attached server), images too.
pub fn paths_from_paste(
    value: &str,
    attached: &[String],
    language: crate::locale::Language,
    uploads: bool,
) -> Result<Option<Dropped>, Refusal> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let whole = decode_path(trimmed).filter(|path| path.is_file());
    let (candidates, text) = if let Some(path) = whole {
        if !is_media(&path) {
            return Ok(None);
        }
        (vec![path], String::new())
    } else {
        let mut media = Vec::new();
        let mut others = Vec::new();
        for word in shellish_words(trimmed) {
            let Some(path) = word_path(&word.text).filter(|path| path.is_file()) else {
                return Ok(None);
            };
            if is_media(&path) {
                media.push(path);
            } else {
                others.push((word.raw, path.is_absolute()));
            }
        }
        if media.is_empty() {
            return Ok(None);
        }
        // Text that only names files, such as a command run on an image or
        // a list of changed files, is not a drop: a terminal writes a drop
        // on one line, every path absolute.
        let drop = !trimmed.contains(['\n', '\r'])
            && media.iter().all(|path| path.is_absolute())
            && others.iter().all(|(_, absolute)| *absolute);
        if !others.is_empty() && !drop {
            return Ok(None);
        }
        let mut text = others
            .iter()
            .map(|(raw, _)| *raw)
            .collect::<Vec<_>>()
            .join(" ");
        // The space a terminal may end a drop with stays after the paths,
        // so what is typed next does not run into them.
        if !text.is_empty() && value.ends_with(|character: char| character.is_ascii_whitespace()) {
            text.push(' ');
        }
        (media, text)
    };
    let keep_text = !text.is_empty();
    match attachable(candidates, attached, language, uploads) {
        Ok(media) => Ok(Some(Dropped { media, text })),
        Err(reason) => Err(Refusal { reason, keep_text }),
    }
}

/// The canonical paths of dropped media, refused when they break a limit of
/// one message. Only media count: other files dropped with them are text.
fn attachable(
    candidates: Vec<PathBuf>,
    attached: &[String],
    language: crate::locale::Language,
    uploads: bool,
) -> Result<Vec<String>, String> {
    if candidates.len() > MAX_ATTACHMENTS {
        return Err(format!("Attach at most {MAX_ATTACHMENTS} files at once"));
    }
    // Counted as the server counts the message, what is attached already
    // included. Images are left out: it shrinks each to a few MB, so only
    // other media can take a message past the limit here. Uploaded, an
    // image goes whole.
    let counts = |source: &str| uploads || !is_image(source);
    let mut total = attached
        .iter()
        .filter(|source| counts(source))
        .map(|source| attached_bytes(source))
        .fold(0_u64, u64::saturating_add);
    let mut result: Vec<String> = Vec::new();
    for path in candidates {
        let size = path
            .metadata()
            .map_err(|error| format!("Cannot read {}: {error}", path.display()))?
            .len();
        let canonical = dunce::canonicalize(&path)
            .map_err(|error| format!("Cannot resolve {}: {error}", path.display()))?
            .display()
            .to_string();
        let counted = attached.contains(&canonical) || result.contains(&canonical);
        if !counted && counts(&canonical) {
            if size > MAX_INLINE_BYTES {
                let name = path.file_name().map_or_else(
                    || path.display().to_string(),
                    |name| name.to_string_lossy().into_owned(),
                );
                return Err(file_over_limit(language, &name, size));
            }
            total = total.saturating_add(size);
            if total > MAX_INLINE_BYTES {
                return Err(over_limit(language, total));
            }
        }
        result.push(canonical);
    }
    Ok(result)
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

/// The file a word of a drop names. Its quotes and escapes were read with
/// the word, so a backslash left in it is part of the name.
fn word_path(text: &str) -> Option<PathBuf> {
    if text.starts_with("file://") {
        return url::Url::parse(text).ok()?.to_file_path().ok();
    }
    Some(PathBuf::from(text))
}

/// Whether a backslash escapes the next character in a dropped path. On
/// Windows it separates folders, and terminals quote a path with a space.
const BACKSLASH_ESCAPES: bool = !cfg!(windows);

/// One word of a drop: what it says once its quotes and escapes are read,
/// and the text the terminal wrote for it.
struct Word<'a> {
    text: String,
    raw: &'a str,
}

/// Small shell-word reader for the forms terminal emulators emit when files
/// are dropped: quoted paths or spaces escaped with a backslash. A word ends
/// at an ASCII space, tab or line break, the blanks a terminal escapes in a
/// path, and nowhere else: the name macOS gives a screenshot keeps a narrow
/// no-break space before AM or PM, unescaped.
fn shellish_words(value: &str) -> Vec<Word<'_>> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut start = None;
    let mut quote = None;
    let mut escaped = false;
    for (index, character) in value.char_indices() {
        if escaped {
            current.push(character);
            escaped = false;
            continue;
        }
        if character.is_ascii_whitespace() && quote.is_none() {
            if let Some(start) = start.take()
                && !current.is_empty()
            {
                words.push(Word {
                    text: std::mem::take(&mut current),
                    raw: &value[start..index],
                });
            }
            continue;
        }
        start.get_or_insert(index);
        if BACKSLASH_ESCAPES && character == '\\' && quote != Some('\'') {
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
        current.push(character);
    }
    if escaped {
        current.push('\\');
    }
    if let Some(start) = start
        && !current.is_empty()
    {
        words.push(Word {
            text: current,
            raw: &value[start..],
        });
    }
    words
}

fn unescape(value: &str) -> String {
    if !BACKSLASH_ESCAPES {
        return value.to_owned();
    }
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
            paths_from_paste("/definitely/not/a/file.png", &[], Language::English, false).unwrap(),
            None
        );
    }

    /// What each word says, and the text the terminal wrote for it.
    fn words(value: &str) -> Vec<(String, &str)> {
        shellish_words(value)
            .into_iter()
            .map(|word| (word.text, word.raw))
            .collect()
    }

    /// A path as a terminal drops it: its spaces escaped, or the whole path
    /// quoted on Windows.
    fn as_dropped(path: &Path) -> String {
        if cfg!(windows) {
            format!("\"{}\"", path.display())
        } else {
            path.display().to_string().replace(' ', "\\ ")
        }
    }

    fn canonical(path: &Path) -> String {
        dunce::canonicalize(path).unwrap().display().to_string()
    }

    /// A paste of media alone that a limit refuses: nothing goes in as text.
    fn refused(reason: impl Into<String>) -> Result<Option<Dropped>, Refusal> {
        Err(Refusal {
            reason: reason.into(),
            keep_text: false,
        })
    }

    #[cfg(not(windows))]
    #[test]
    fn shellish_words_handle_terminal_drop_escaping() {
        assert_eq!(
            words("/tmp/one\\ file.png '/tmp/two file.mp4'"),
            vec![
                ("/tmp/one file.png".into(), "/tmp/one\\ file.png"),
                ("/tmp/two file.mp4".into(), "'/tmp/two file.mp4'"),
            ]
        );
    }

    /// Windows terminals quote a dropped path with a space, and its
    /// backslashes separate folders.
    #[cfg(windows)]
    #[test]
    fn shellish_words_read_windows_drops() {
        assert_eq!(
            words(r#""C:\shots\one file.png" C:\clips\two.mp4"#),
            vec![
                (
                    r"C:\shots\one file.png".into(),
                    r#""C:\shots\one file.png""#
                ),
                (r"C:\clips\two.mp4".into(), r"C:\clips\two.mp4"),
            ]
        );
    }

    /// A terminal escapes ASCII spaces alone, so the narrow no-break space
    /// macOS writes before a screenshot's AM or PM, like any other Unicode
    /// space, is part of the word.
    #[test]
    fn a_unicode_space_is_part_of_a_dropped_word() {
        assert_eq!(
            words("/tmp/9.41.00\u{202f}AM.png\t/tmp/a\u{a0}b.png\n/tmp/c\u{3000}d.png  /tmp/e.png"),
            vec![
                (
                    "/tmp/9.41.00\u{202f}AM.png".into(),
                    "/tmp/9.41.00\u{202f}AM.png"
                ),
                ("/tmp/a\u{a0}b.png".into(), "/tmp/a\u{a0}b.png"),
                ("/tmp/c\u{3000}d.png".into(), "/tmp/c\u{3000}d.png"),
                ("/tmp/e.png".into(), "/tmp/e.png"),
            ]
        );
        #[cfg(not(windows))]
        assert_eq!(
            words(
                "/tmp/Screenshot\\ at\\ 9.41.00\u{202f}AM.png /tmp/Screenshot\\ at\\ 9.41.05\u{202f}PM.png"
            ),
            vec![
                (
                    "/tmp/Screenshot at 9.41.00\u{202f}AM.png".into(),
                    "/tmp/Screenshot\\ at\\ 9.41.00\u{202f}AM.png"
                ),
                (
                    "/tmp/Screenshot at 9.41.05\u{202f}PM.png".into(),
                    "/tmp/Screenshot\\ at\\ 9.41.05\u{202f}PM.png"
                ),
            ]
        );
    }

    #[test]
    fn recognizes_a_real_dropped_image_path() {
        let root = std::env::temp_dir().join(format!("haarness-media-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&root).unwrap();
        let image = root.join("screen shot.png");
        std::fs::write(&image, b"png fixture").unwrap();

        assert_eq!(
            paths_from_paste(&as_dropped(&image), &[], Language::English, false),
            Ok(Some(Dropped {
                media: vec![canonical(&image)],
                text: String::new(),
            }))
        );

        let _ = std::fs::remove_dir_all(root);
    }

    /// Two macOS screenshots dropped at once: their names' spaces escaped,
    /// the narrow no-break space before AM not.
    #[test]
    fn two_dropped_screenshots_both_attach() {
        let root = std::env::temp_dir().join(format!("haarness-shots-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&root).unwrap();
        let shots: Vec<_> = ["9.41.00", "9.41.05"]
            .iter()
            .map(|time| {
                let shot = root.join(format!("Screenshot 2026-11-19 at {time}\u{202f}AM.png"));
                std::fs::write(&shot, b"png fixture").unwrap();
                shot
            })
            .collect();

        let drop = format!("{} {} ", as_dropped(&shots[0]), as_dropped(&shots[1]));
        assert_eq!(
            paths_from_paste(&drop, &[], Language::English, false),
            Ok(Some(Dropped {
                media: shots.iter().map(|shot| canonical(shot)).collect(),
                text: String::new(),
            }))
        );

        let _ = std::fs::remove_dir_all(root);
    }

    /// A drop of media and other files attaches the media; the others stay
    /// their paths, as the terminal wrote them, and only the media count
    /// toward the limits.
    #[test]
    fn a_drop_of_media_and_other_files_attaches_only_the_media() {
        let root = std::env::temp_dir().join(format!("haarness-mixed-{}", uuid::Uuid::now_v7()));
        let notes = root.join("team docs");
        std::fs::create_dir_all(&notes).unwrap();
        let image = root.join("screen shot.png");
        std::fs::write(&image, b"png fixture").unwrap();
        let readme = notes.join("README.md");
        std::fs::write(&readme, "# Notes\n").unwrap();
        let log = root.join("build.log");
        std::fs::write(&log, "ok\n").unwrap();

        let drop = format!(
            "{} {} {}",
            as_dropped(&readme),
            as_dropped(&image),
            as_dropped(&log)
        );
        assert_eq!(
            paths_from_paste(&drop, &[], Language::English, false),
            Ok(Some(Dropped {
                media: vec![canonical(&image)],
                text: format!("{} {}", as_dropped(&readme), as_dropped(&log)),
            }))
        );
        // A space the terminal ended the drop with stays after the paths.
        assert_eq!(
            paths_from_paste(&format!("{drop} "), &[], Language::English, false)
                .unwrap()
                .unwrap()
                .text,
            format!("{} {} ", as_dropped(&readme), as_dropped(&log))
        );

        // Words that are not all existing files are text, as before.
        for text in [
            format!("{} hello", as_dropped(&image)),
            format!("{} {}", as_dropped(&image), root.join("gone.md").display()),
            format!("{} {}", as_dropped(&image), as_dropped(&notes)),
            format!("{} {}", as_dropped(&readme), as_dropped(&log)),
        ] {
            assert_eq!(
                paths_from_paste(&text, &[], Language::English, false),
                Ok(None),
                "{text}"
            );
        }

        // Eight images and a large file that is not media: only the images
        // count, toward the number of files and the size of a message.
        let shots: Vec<_> = (1..=9)
            .map(|index| {
                let shot = root.join(format!("shot {index}.png"));
                std::fs::write(&shot, b"png fixture").unwrap();
                shot
            })
            .collect();
        std::fs::File::create(&log)
            .unwrap()
            .set_len(MAX_INLINE_BYTES + 1)
            .unwrap();
        let eight = shots[..8]
            .iter()
            .chain([&log])
            .map(|path| as_dropped(path))
            .collect::<Vec<_>>()
            .join(" ");
        let dropped = paths_from_paste(&eight, &[], Language::English, true)
            .unwrap()
            .unwrap();
        assert_eq!(dropped.media.len(), 8);
        assert_eq!(dropped.text, as_dropped(&log));
        let nine = shots
            .iter()
            .map(|path| as_dropped(path))
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(
            paths_from_paste(&nine, &[], Language::English, false),
            refused("Attach at most 8 files at once")
        );

        let _ = std::fs::remove_dir_all(root);
    }

    /// `path` as seen from the working folder, through `..`: how a command
    /// or a list of the project's files names a file.
    fn relative(path: &Path) -> PathBuf {
        let up = std::env::current_dir()
            .unwrap()
            .components()
            .filter(|part| matches!(part, std::path::Component::Normal(_)))
            .count();
        let mut relative: PathBuf = std::iter::repeat_n("..", up).collect();
        relative.extend(
            path.components()
                .filter(|part| matches!(part, std::path::Component::Normal(_))),
        );
        relative
    }

    /// Text that names files without being a drop, such as a command run on
    /// an image or a list of changed files, stays text when it mixes media
    /// and other files. Media alone are attached, as they always were.
    #[test]
    fn a_paste_that_only_names_files_is_not_a_drop() {
        let root = std::env::temp_dir().join(format!("haarness-named-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&root).unwrap();
        let script = root.join("resize.sh");
        let image = root.join("icon.png");
        let other = root.join("logo.png");
        let readme = root.join("README.md");
        for file in [&script, &image, &other] {
            std::fs::write(file, b"fixture").unwrap();
        }
        std::fs::write(&readme, "# Notes\n").unwrap();
        assert!(relative(&image).is_file(), "{}", relative(&image).display());

        for text in [
            // A command run on an image, its words relative.
            format!(
                "{} {}",
                relative(&script).display(),
                relative(&image).display()
            ),
            format!("{} {}", relative(&script).display(), image.display()),
            // A list of files, one a line, as `git diff --name-only` prints.
            format!(
                "{}\n{}",
                relative(&readme).display(),
                relative(&image).display()
            ),
            format!("{}\n{}", readme.display(), image.display()),
            format!("{}\r\n{}", image.display(), readme.display()),
        ] {
            assert_eq!(
                paths_from_paste(&text, &[], Language::English, false),
                Ok(None),
                "{text:?}"
            );
        }
        // Media alone are attached however they are named.
        for text in [
            format!(
                "{} {}",
                relative(&image).display(),
                relative(&other).display()
            ),
            format!("{}\n{}", image.display(), other.display()),
        ] {
            assert_eq!(
                paths_from_paste(&text, &[], Language::English, false)
                    .unwrap()
                    .map(|dropped| dropped.media.len()),
                Some(2),
                "{text:?}"
            );
        }

        let _ = std::fs::remove_dir_all(root);
    }

    /// A drop whose media break a limit is refused, but when it named other
    /// files too it still goes in as text: none of it is lost.
    #[test]
    fn a_mixed_drop_over_a_limit_keeps_its_text() {
        let root = std::env::temp_dir().join(format!("haarness-limit-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&root).unwrap();
        let readme = root.join("README.md");
        std::fs::write(&readme, "# Notes\n").unwrap();
        let shots: Vec<_> = (1..=9)
            .map(|index| {
                let shot = root.join(format!("shot {index}.png"));
                std::fs::write(&shot, b"png fixture").unwrap();
                shot
            })
            .collect();
        let video = root.join("demo.mov");
        std::fs::File::create(&video)
            .unwrap()
            .set_len(25 * 1024 * 1024)
            .unwrap();

        let nine = shots
            .iter()
            .chain([&readme])
            .map(|path| as_dropped(path))
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(
            paths_from_paste(&nine, &[], Language::English, false),
            Err(Refusal {
                reason: "Attach at most 8 files at once".into(),
                keep_text: true,
            })
        );
        let large = format!("{} {}", as_dropped(&video), as_dropped(&readme));
        assert_eq!(
            paths_from_paste(&large, &[], Language::English, false),
            Err(Refusal {
                reason: "demo.mov is 25 MiB, over the 20 MiB limit for media in one message".into(),
                keep_text: true,
            })
        );

        let _ = std::fs::remove_dir_all(root);
    }

    /// A name holding a backslash, which a terminal writes as `\\`, keeps
    /// it when it comes with other files: each word is unescaped once.
    #[cfg(not(windows))]
    #[test]
    fn a_backslash_in_a_dropped_name_is_read_once() {
        let root = std::env::temp_dir().join(format!("haarness-slash-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&root).unwrap();
        let odd = root.join("before\\after.png");
        let shot = root.join("shot.png");
        for file in [&odd, &shot] {
            std::fs::write(file, b"png fixture").unwrap();
        }
        let escaped = odd.display().to_string().replace('\\', "\\\\");

        for drop in [escaped.clone(), format!("{escaped} {}", shot.display())] {
            assert_eq!(
                paths_from_paste(&drop, &[], Language::English, false)
                    .unwrap()
                    .map(|dropped| dropped.media.first().cloned()),
                Some(Some(canonical(&odd))),
                "{drop}"
            );
        }

        let _ = std::fs::remove_dir_all(root);
    }

    /// Windows terminals quote each dropped path; a quoted path that stays
    /// text keeps its quotes.
    #[test]
    fn a_quoted_mixed_drop_keeps_the_other_files_quoted() {
        let root = std::env::temp_dir().join(format!("haarness-quoted-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&root).unwrap();
        let image = root.join("screen shot.png");
        std::fs::write(&image, b"png fixture").unwrap();
        let readme = root.join("read me.md");
        std::fs::write(&readme, "# Notes\n").unwrap();

        let drop = format!("\"{}\" \"{}\"", image.display(), readme.display());
        assert_eq!(
            paths_from_paste(&drop, &[], Language::English, false),
            Ok(Some(Dropped {
                media: vec![canonical(&image)],
                text: format!("\"{}\"", readme.display()),
            }))
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
            paths_from_paste(image.to_str().unwrap(), &[], Language::English, false)
                .unwrap()
                .is_some()
        );
        assert!(
            inline_sources(vec![image.display().to_string()])
                .await
                .is_ok()
        );
        // The server shrinks an image, so a paste takes a large one; one
        // uploaded to an attached server goes whole, so it is refused there
        // when it is dropped, by name.
        file.set_len(20 * 1024 * 1024 + 1).unwrap();
        assert!(
            paths_from_paste(image.to_str().unwrap(), &[], Language::English, false)
                .unwrap()
                .is_some()
        );
        let named = "screen shot.png is 20.1 MiB, over the 20 MiB limit for media in one message";
        assert_eq!(
            paths_from_paste(image.to_str().unwrap(), &[], Language::English, true),
            refused(named)
        );
        let error = inline_sources(vec![image.display().to_string()])
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), named);

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
            paths_from_paste(readme.to_str().unwrap(), &[], Language::English, false),
            Ok(None)
        );
        // Together with media it stays text too, and the media attaches.
        let both = format!("{} {}", image.display(), readme.display());
        assert_eq!(
            paths_from_paste(&both, &[], Language::English, false),
            Ok(Some(Dropped {
                media: vec![canonical(&image)],
                text: readme.display().to_string(),
            }))
        );

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
            paths_from_paste(pointer.to_str().unwrap(), &[], Language::English, false),
            Ok(None)
        );

        // A real one, its header after a little junk as readers allow.
        let real = root.join("report.pdf");
        std::fs::write(&real, b"\r\n%PDF-1.7\n%%EOF\n").unwrap();
        assert_eq!(
            paths_from_paste(real.to_str().unwrap(), &[], Language::English, false),
            Ok(Some(Dropped {
                media: vec![canonical(&real)],
                text: String::new(),
            }))
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
        let first = paths_from_paste(shots[0].to_str().unwrap(), &[], Language::English, false)
            .unwrap()
            .unwrap()
            .media;
        assert!(
            paths_from_paste(shots[1].to_str().unwrap(), &first, Language::English, false)
                .unwrap()
                .is_some()
        );
        let both = format!("{} {}", shots[0].display(), shots[1].display());
        assert_eq!(
            paths_from_paste(&both, &[], Language::English, false)
                .unwrap()
                .map(|dropped| dropped.media.len()),
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
        let attached = vec![dunce::canonicalize(&first).unwrap().display().to_string()];
        assert!(
            paths_from_paste(second.to_str().unwrap(), &[], Language::English, false)
                .unwrap()
                .is_some()
        );
        assert_eq!(
            paths_from_paste(
                second.to_str().unwrap(),
                &attached,
                Language::English,
                false
            ),
            refused(
                "The attachments come to 24 MiB, over the 20 MiB limit for media in one message"
            )
        );
        // In the interface language.
        assert_eq!(
            paths_from_paste(second.to_str().unwrap(), &attached, Language::German, false),
            refused(Language::German.format(
                "The attachments come to {size}, over the {limit} limit for media in one message",
                &[("size", "24 MiB"), ("limit", "20 MiB")]
            ))
        );
        // A file attached already is not counted twice.
        let again = format!("{} {}", first.display(), second.display());
        assert!(paths_from_paste(&again, &attached, Language::English, false).is_err());
        assert!(
            paths_from_paste(first.to_str().unwrap(), &attached, Language::English, false)
                .unwrap()
                .is_some()
        );
        // A data URL already attached (a picked server file) counts as well.
        let picked = format!(
            "data:application/pdf;base64,{}",
            STANDARD.encode(vec![0_u8; 12 * 1024 * 1024])
        );
        assert!(
            paths_from_paste(
                second.to_str().unwrap(),
                &[picked],
                Language::English,
                false
            )
            .is_err()
        );

        let _ = std::fs::remove_dir_all(root);
    }
}

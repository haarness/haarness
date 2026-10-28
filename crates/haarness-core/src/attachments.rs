//! What a person may attach to a message, and how attachments reach a model.
//!
//! Providers take images, PDFs, audio and video. A text file (notes, code,
//! JSON) is read into the conversation as text instead, so a client may
//! attach any file a person would share. Anything else is refused before the
//! message is stored: a stored attachment the provider rejects would fail
//! every later turn of that chat. Images are checked by their bytes and made
//! into ones every provider reads (see [`crate::images`]); a message's media
//! stays within [`MAX_INLINE_MEDIA_BYTES`].

use std::collections::HashSet;
use std::fmt::Write as _;
use std::io::Read as _;
use std::path::{Path, PathBuf};

use base64::{Engine as _, engine::general_purpose::STANDARD};

pub use haarness_protocol::MAX_INLINE_MEDIA_BYTES;
use haarness_protocol::mebibytes;

use crate::{CoreError, CoreResult};

/// The largest text file read into a message.
pub const MAX_TEXT_ATTACHMENT_BYTES: usize = 512 * 1024;

/// The largest image file read for conversion; what is kept is far smaller.
const MAX_IMAGE_SOURCE_BYTES: u64 = 64 * 1024 * 1024;

/// How far into a file PDF readers look for its `%PDF-` header.
const PDF_HEADER_WINDOW: usize = 1024;

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

/// Checks a new message's attachments. Images are made into ones every
/// provider reads, other media stays as sent; a text file named by path is
/// carried as a text data URL, so the message keeps it even if the file
/// changes; anything a model cannot read is refused, and so is media past
/// [`MAX_INLINE_MEDIA_BYTES`], one file or all of them together.
pub fn admit(attachments: Vec<String>, workspace: &Path) -> CoreResult<Vec<String>> {
    let admitted = attachments
        .into_iter()
        .map(|source| admit_one(source, workspace))
        .collect::<CoreResult<Vec<_>>>()?;
    within_limit(&admitted)?;
    Ok(admitted)
}

/// Refuses admitted `sources` whose media together pass
/// [`MAX_INLINE_MEDIA_BYTES`].
pub fn within_limit(sources: &[String]) -> CoreResult<()> {
    let total = admitted_media_bytes(sources);
    if total > MAX_INLINE_MEDIA_BYTES as u64 {
        return Err(CoreError::Invalid(format!(
            "The attachments come to {}, over the {} limit for media in one message",
            mebibytes(total),
            mebibytes(MAX_INLINE_MEDIA_BYTES as u64)
        )));
    }
    Ok(())
}

/// [`admit`] for a message admitted once already, when its turn comes (a
/// queued message): each attachment on its own, and none refused. One whose
/// file has changed since (grown past the limit, gone, no longer a PDF)
/// stays as it was, a relative path made absolute, for the request to name
/// in a note (see [`for_model`]) instead of holding back the queue.
pub fn admit_each(attachments: Vec<String>, workspace: &Path) -> Vec<String> {
    attachments
        .into_iter()
        .map(|source| {
            admit_one(source.clone(), workspace).unwrap_or_else(|_| {
                match file_path(&source).filter(|path| path.is_relative()) {
                    Some(path) => workspace.join(path).to_string_lossy().into_owned(),
                    None => source,
                }
            })
        })
        .collect()
}

/// [`admit`] on a blocking thread: converting an image is CPU work.
pub async fn admit_off_executor(
    attachments: Vec<String>,
    workspace: PathBuf,
) -> CoreResult<Vec<String>> {
    if attachments.is_empty() {
        return Ok(attachments);
    }
    tokio::task::spawn_blocking(move || admit(attachments, &workspace))
        .await
        .map_err(|error| CoreError::Invalid(format!("attachments could not be checked: {error}")))?
}

fn admit_one(source: String, workspace: &Path) -> CoreResult<String> {
    if let Some(data) = source.strip_prefix("data:") {
        let url = DataUrl::parse(data)?;
        if url.mime_type.starts_with("image/") {
            let bytes = url.bytes()?;
            let declared = url.mime_type.to_owned();
            return admit_image(bytes, url.file_name(), |actual| {
                (actual == declared).then(|| source.clone())
            });
        }
        if is_supported_media_mime(url.mime_type) {
            let size = url.decoded_len();
            if size > MAX_INLINE_MEDIA_BYTES as u64 {
                return Err(over_limit(&url.name(), size));
            }
            if url.mime_type != "application/pdf" {
                return Ok(source);
            }
            let bytes = url.bytes()?;
            if starts_like_pdf(&bytes) {
                return Ok(source);
            }
            return admit_mislabelled_pdf(bytes, url.file_name());
        }
        let bytes = url.bytes()?;
        if text(&bytes, url.mime_type).is_none() {
            // An image sent without its type (a pasted screenshot) is still one.
            if crate::images::sniff(&bytes).is_some() {
                return admit_image(bytes, url.file_name(), |_| None);
            }
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
    let name = path.file_name().map_or_else(
        || "attachment".into(),
        |name| name.to_string_lossy().into_owned(),
    );
    let stored = || {
        if relative {
            path.to_string_lossy().into_owned()
        } else {
            source.clone()
        }
    };
    // A path whose name gives the wrong type is still sent as what it holds:
    // the provider adapters read an image's type from its bytes.
    let keep_path = |_: &str| Some(stored());
    let mime_type = media_mime_for_path(&path);
    if mime_type.is_some_and(|mime_type| mime_type.starts_with("image/")) {
        if metadata.len() > MAX_IMAGE_SOURCE_BYTES {
            return Err(CoreError::Invalid(format!(
                "{name} is larger than the {} MiB image limit",
                MAX_IMAGE_SOURCE_BYTES / 1024 / 1024
            )));
        }
        let bytes = read(&path, &name)?;
        return admit_image(bytes, Some(name), keep_path);
    }
    if mime_type == Some("application/pdf") {
        let head = read_at_most(&path, PDF_HEADER_WINDOW).map_err(|error| {
            CoreError::Invalid(format!("cannot read attachment {name}: {error}"))
        })?;
        if !starts_like_pdf(&head) {
            if metadata.len() > MAX_TEXT_ATTACHMENT_BYTES as u64 {
                return Err(not_a_pdf(&name));
            }
            return admit_mislabelled_pdf(read(&path, &name)?, Some(name));
        }
    }
    if mime_type.is_some() {
        if metadata.len() > MAX_INLINE_MEDIA_BYTES as u64 {
            return Err(over_limit(&name, metadata.len()));
        }
        return Ok(stored());
    }
    if usize::try_from(metadata.len()).unwrap_or(usize::MAX) > MAX_TEXT_ATTACHMENT_BYTES {
        return Err(too_large(&name));
    }
    let bytes = read(&path, &name)?;
    if text(&bytes, "").is_none() {
        if crate::images::sniff(&bytes).is_some() {
            return admit_image(bytes, Some(name), keep_path);
        }
        return Err(unsupported(&name));
    }
    Ok(text_data_url(Some(&name), &bytes))
}

/// A file named or labelled a PDF whose bytes do not start like one (a Git
/// LFS pointer, an error page saved under the name) is taken for what it
/// holds, as an untyped file would be: text, or an image, or refused. Sent as
/// a PDF, the provider would refuse it on every later turn of the chat.
fn admit_mislabelled_pdf(bytes: Vec<u8>, name: Option<String>) -> CoreResult<String> {
    let label = name.clone().unwrap_or_else(|| "attachment.pdf".into());
    if text(&bytes, "").is_some() {
        if bytes.len() > MAX_TEXT_ATTACHMENT_BYTES {
            return Err(too_large(&label));
        }
        return Ok(text_data_url(name.as_deref(), &bytes));
    }
    // Kept inline, typed by its bytes: by path its name would say PDF again.
    if crate::images::sniff(&bytes).is_some() {
        return admit_image(bytes, name, |_| None);
    }
    Err(not_a_pdf(&label))
}

fn text_data_url(name: Option<&str>, bytes: &[u8]) -> String {
    let name = name
        .map(|name| {
            format!(
                ";name={}",
                url::form_urlencoded::byte_serialize(name.as_bytes()).collect::<String>()
            )
        })
        .unwrap_or_default();
    format!("data:text/plain{name};base64,{}", STANDARD.encode(bytes))
}

/// Whether bytes hold a PDF: readers take one whose `%PDF-` header lies
/// within its first kilobyte, after any junk a mail or web gateway added.
fn starts_like_pdf(bytes: &[u8]) -> bool {
    bytes[..bytes.len().min(PDF_HEADER_WINDOW)]
        .windows(5)
        .any(|part| part == b"%PDF-")
}

/// An image as every provider reads it. One that already is may stay as it
/// was attached (`keep` decides, given its actual type); a converted, shrunk
/// or mislabelled one is kept inline, so the model sees what was checked.
fn admit_image(
    bytes: Vec<u8>,
    name: Option<String>,
    keep: impl FnOnce(&str) -> Option<String>,
) -> CoreResult<String> {
    let label = name.clone().unwrap_or_else(|| "The image".into());
    let prepared = crate::images::prepare(bytes)
        .map_err(|refusal| CoreError::Invalid(refusal.message(&label)))?;
    if !prepared.changed
        && let Some(kept) = keep(prepared.mime_type)
    {
        return Ok(kept);
    }
    let name = name
        .map(|name| {
            format!(
                ";name={}",
                url::form_urlencoded::byte_serialize(name.as_bytes()).collect::<String>()
            )
        })
        .unwrap_or_default();
    Ok(format!(
        "data:{}{name};base64,{}",
        prepared.mime_type,
        STANDARD.encode(prepared.bytes)
    ))
}

fn read(path: &Path, name: &str) -> CoreResult<Vec<u8>> {
    std::fs::read(path)
        .map_err(|error| CoreError::Invalid(format!("cannot read attachment {name}: {error}")))
}

/// A file's bytes up to one past `limit`: more than `limit` says the file is
/// over it (it may have grown since it was measured) without reading it all.
pub fn read_at_most(path: &Path, limit: usize) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1))
        .read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// About how much of [`MAX_INLINE_MEDIA_BYTES`] `sources` take once
/// admitted, read before admitting them: an image at most what it is shrunk
/// to, other media its size, a text file nothing. A relative path is read
/// from `workspace`; a file that cannot be read counts nothing (admitting it
/// refuses it).
#[must_use]
pub fn media_estimate(sources: &[String], workspace: &Path) -> u64 {
    let cap = crate::images::MAX_IMAGE_BYTES as u64;
    sources
        .iter()
        .map(|source| {
            let image = is_image_source(source);
            let size = if source.starts_with("data:") {
                media_bytes(source).unwrap_or(0)
            } else {
                local_path(source)
                    .map(|path| workspace.join(path))
                    .filter(|path| media_mime_for_path(path).is_some())
                    .and_then(|path| std::fs::metadata(path).ok())
                    .filter(std::fs::Metadata::is_file)
                    .map_or(0, |metadata| metadata.len())
            };
            if image { size.min(cap) } else { size }
        })
        .fold(0, u64::saturating_add)
}

/// What admitted `sources` take of [`MAX_INLINE_MEDIA_BYTES`], as [`admit`]
/// counts them: each media data URL's payload and file's length.
#[must_use]
pub fn admitted_media_bytes(sources: &[String]) -> u64 {
    sources
        .iter()
        .filter_map(|source| media_bytes(source))
        .fold(0, u64::saturating_add)
}

/// The bytes a provider loads for a stored media source: a media data URL's
/// payload or a file's length. `None` for a text file, which reaches the
/// model as text, and for a file that is gone.
fn media_bytes(source: &str) -> Option<u64> {
    if let Some(data) = source.strip_prefix("data:") {
        return DataUrl::parse(data)
            .ok()
            .filter(|url| is_supported_media_mime(url.mime_type))
            .map(|url| url.decoded_len());
    }
    local_path(source)
        .and_then(|path| std::fs::metadata(path).ok())
        .filter(std::fs::Metadata::is_file)
        .map(|metadata| metadata.len())
}

/// A stable identity for a stored attachment source (a path or a data URL),
/// recorded when a provider refuses it so later requests leave it out.
#[must_use]
pub fn fingerprint(source: &str) -> String {
    use sha2::Digest as _;
    hex::encode(sha2::Sha256::digest(source.as_bytes()))
}

/// What a person calls an attachment: its file name, or its type.
#[must_use]
pub fn display_name(source: &str) -> String {
    if let Some(data) = source.strip_prefix("data:") {
        return DataUrl::parse(data).map_or_else(
            |_| "attachment".into(),
            |url| url.file_name().unwrap_or_else(|| url.mime_type.to_owned()),
        );
    }
    local_path(source)
        .and_then(|path| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| source.chars().take(256).collect())
}

/// True for a source that holds an image, judged by its type or extension.
#[must_use]
pub fn is_image_source(source: &str) -> bool {
    if let Some(data) = source.strip_prefix("data:") {
        return data.starts_with("image/");
    }
    local_path(source)
        .and_then(|path| media_mime_for_path(&path))
        .is_some_and(|mime_type| mime_type.starts_with("image/"))
}

/// The file an attachment source names, when it is a file rather than a data
/// URL or a web address.
#[must_use]
pub fn file_path(source: &str) -> Option<PathBuf> {
    if source.starts_with("data:")
        || source.starts_with("http://")
        || source.starts_with("https://")
    {
        return None;
    }
    local_path(source)
}

/// The bytes a provider receives for an attachment source: the base64 of a
/// data URL or of a file, or the address of an image it fetches itself. A
/// file that cannot be read counts nothing.
#[must_use]
pub fn sent_bytes(source: &str) -> usize {
    if let Some(data) = source.strip_prefix("data:") {
        return data.split_once(',').map_or(0, |(_, encoded)| encoded.len());
    }
    if source.starts_with("http://") || source.starts_with("https://") {
        return source.len();
    }
    local_path(source)
        .and_then(|path| std::fs::metadata(path).ok())
        .map_or(0, |metadata| {
            usize::try_from(metadata.len())
                .unwrap_or(usize::MAX)
                .div_ceil(3)
                .saturating_mul(4)
        })
}

/// A stored message as a model receives it: text files are appended to the
/// message, and media that can no longer be read (an upload folder deleted
/// since) is named instead of failing the turn. So is media that would take
/// the message past [`MAX_INLINE_MEDIA_BYTES`], and a PDF that does not hold
/// one: stored before either was refused, or a file changed since (grown, or
/// an LFS pointer after a checkout), it would fail every turn after it.
/// Sources a provider refused (their [`fingerprint`]s in `omitted`) pass
/// through for the caller to leave out, and take no room.
pub fn for_model<S: std::hash::BuildHasher>(
    content: String,
    attachments: Vec<String>,
    omitted: &HashSet<String, S>,
) -> (String, Vec<String>) {
    let mut content = content;
    let mut media = Vec::new();
    let mut carried = 0_u64;
    let mut fits = |content: &mut String, label: &str, size: u64| {
        let total = carried.saturating_add(size);
        if total > MAX_INLINE_MEDIA_BYTES as u64 {
            let _ = write!(
                content,
                "\n[Attachment {label} left out: it would bring this message's media to {}, over the {} limit for one message]",
                mebibytes(total),
                mebibytes(MAX_INLINE_MEDIA_BYTES as u64)
            );
            return false;
        }
        carried = total;
        true
    };
    let refused = |source: &str| !omitted.is_empty() && omitted.contains(&fingerprint(source));
    let not_a_pdf = |content: &mut String, label: &str| {
        let _ = write!(
            content,
            "\n[Attachment {label} left out: it does not hold a readable PDF]"
        );
    };
    for source in attachments {
        if let Some(data) = source.strip_prefix("data:") {
            match DataUrl::parse(data) {
                Ok(url) if is_supported_media_mime(url.mime_type) => {
                    if refused(&source) {
                        media.push(source);
                    } else if url.mime_type == "application/pdf"
                        && !url.head().is_some_and(|head| starts_like_pdf(&head))
                    {
                        not_a_pdf(&mut content, &url.name());
                    } else if fits(&mut content, &url.name(), url.decoded_len()) {
                        media.push(source);
                    }
                }
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
        if source.starts_with("http://") || source.starts_with("https://") {
            media.push(source);
            continue;
        }
        // Gone, or no longer readable (its permissions changed): the provider
        // could not load it either.
        let file = local_path(&source).and_then(|path| {
            let metadata = std::fs::metadata(&path).ok()?;
            let readable = metadata.is_file() && std::fs::File::open(&path).is_ok();
            readable.then_some((path, metadata.len()))
        });
        let Some((path, size)) = file else {
            let _ = write!(content, "\n[Attachment {source} is no longer available]");
            continue;
        };
        if refused(&source) {
            media.push(source);
        } else if media_mime_for_path(&path) == Some("application/pdf")
            && !read_at_most(&path, PDF_HEADER_WINDOW).is_ok_and(|head| starts_like_pdf(&head))
        {
            not_a_pdf(&mut content, &source);
        } else if fits(&mut content, &source, size) {
            media.push(source);
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

    /// The payload's first bytes, as many as a PDF's header may lie within,
    /// decoded without the rest.
    fn head(&self) -> Option<Vec<u8>> {
        let encoded = self
            .encoded
            .get(..PDF_HEADER_WINDOW.div_ceil(3) * 4)
            .unwrap_or(self.encoded);
        STANDARD.decode(encoded).ok()
    }

    /// The payload's length once decoded, without decoding it.
    fn decoded_len(&self) -> u64 {
        let padding = self
            .encoded
            .bytes()
            .rev()
            .take_while(|&byte| byte == b'=')
            .count();
        u64::try_from((self.encoded.len() / 4 * 3).saturating_sub(padding)).unwrap_or(u64::MAX)
    }

    fn bytes(&self) -> CoreResult<Vec<u8>> {
        STANDARD
            .decode(self.encoded)
            .map_err(|error| CoreError::Invalid(format!("invalid attachment base64: {error}")))
    }

    fn name(&self) -> String {
        self.file_name()
            .unwrap_or_else(|| self.mime_type.to_owned())
    }

    fn file_name(&self) -> Option<String> {
        self.parameters
            .iter()
            .find_map(|part| part.strip_prefix("name="))
            .and_then(|name| url::form_urlencoded::parse(name.as_bytes()).next())
            .map(|(name, _)| name.into_owned())
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

fn over_limit(name: &str, size: u64) -> CoreError {
    CoreError::Invalid(format!(
        "{name} is {}, over the {} limit for media in one message",
        mebibytes(size),
        mebibytes(MAX_INLINE_MEDIA_BYTES as u64)
    ))
}

fn not_a_pdf(name: &str) -> CoreError {
    CoreError::Invalid(format!(
        "{name} cannot be attached: it is named as a PDF but does not hold one"
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

    fn image(width: u32, height: u32, format: image::ImageFormat) -> Vec<u8> {
        crate::images::tests::encoded(width, height, format)
    }

    #[test]
    fn media_is_kept_as_sent() {
        let png = data_url("image/png", None, &image(8, 8, image::ImageFormat::Png));
        let pdf = data_url("application/pdf", None, b"%PDF-1.4");
        assert_eq!(
            admit(vec![png.clone(), pdf.clone()], Path::new("/")).unwrap(),
            vec![png, pdf]
        );
    }

    #[test]
    fn a_jpeg_labelled_png_is_sent_as_jpeg() {
        let jpeg = image(16, 16, image::ImageFormat::Jpeg);
        let admitted = admit(
            vec![data_url("image/png", Some("shot.png"), &jpeg)],
            Path::new("/"),
        )
        .unwrap();
        assert_eq!(
            admitted,
            vec![data_url("image/jpeg", Some("shot.png"), &jpeg)]
        );
    }

    #[test]
    fn bmp_and_tiff_become_png_and_big_images_shrink() {
        let dir = std::env::temp_dir().join(format!("haarness-attach-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("scan.bmp"), image(20, 10, image::ImageFormat::Bmp)).unwrap();
        let tiff = data_url(
            "image/tiff",
            Some("fax.tiff"),
            &image(10, 20, image::ImageFormat::Tiff),
        );
        let wide = data_url(
            "image/png",
            Some("wide.png"),
            &image(2600, 40, image::ImageFormat::Png),
        );
        let admitted = admit(vec!["scan.bmp".into(), tiff, wide], &dir).unwrap();
        for (source, name) in admitted.iter().zip(["scan.bmp", "fax.tiff", "wide.png"]) {
            assert!(
                source.starts_with(&format!("data:image/png;name={name};base64,")),
                "{}",
                &source[..60]
            );
        }
        let (_, encoded) = admitted[2].split_once(',').unwrap();
        let wide = ::image::load_from_memory(&STANDARD.decode(encoded).unwrap()).unwrap();
        assert_eq!((wide.width(), wide.height()), (2000, 31));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn images_that_do_not_decode_are_refused_before_they_are_stored() {
        let error = admit(
            vec![data_url(
                "image/png",
                Some("half.png"),
                b"\x89PNG\r\n\x1a\n\0\0",
            )],
            Path::new("/"),
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("half.png cannot be read as an image"),
            "{error}"
        );
        let error = admit(
            vec![data_url("image/png", Some("notes.png"), b"just text")],
            Path::new("/"),
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("notes.png is not an image"),
            "{error}"
        );
    }

    #[test]
    fn fingerprints_and_names_identify_sources() {
        let source = data_url("image/png", Some("a%20b.png"), b"x");
        assert_eq!(fingerprint(&source), fingerprint(&source.clone()));
        assert_ne!(fingerprint(&source), fingerprint("/tmp/a b.png"));
        assert_eq!(fingerprint(&source).len(), 64);
        assert_eq!(display_name(&source), "a b.png");
        assert_eq!(display_name("/tmp/shots/one.png"), "one.png");
        assert!(is_image_source(&source));
        assert!(is_image_source("/tmp/shots/one.PNG"));
        assert!(!is_image_source("/tmp/report.pdf"));
    }

    #[test]
    fn text_files_are_admitted_and_reach_the_model_as_text() {
        let notes = data_url("text/plain", Some("notes.txt"), b"code word PELICAN\n");
        let json = data_url("application/json", Some("a.json"), b"{\"a\":1}");
        let untyped = data_url("application/octet-stream", Some("Makefile"), b"all:\n");
        let admitted = admit(vec![notes, json, untyped], Path::new("/")).unwrap();
        let (content, media) = for_model("Read these".into(), admitted, &HashSet::new());
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
        std::fs::write(dir.join("shot.png"), image(4, 4, image::ImageFormat::Png)).unwrap();
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
            &HashSet::new(),
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

    /// A sparse file of `size` bytes that starts like a PDF.
    fn sparse_pdf(path: &Path, size: u64) {
        let mut file = std::fs::File::create(path).unwrap();
        std::io::Write::write_all(&mut file, b"%PDF-1.7\n").unwrap();
        file.set_len(size).unwrap();
    }

    const MIB: u64 = 1024 * 1024;

    #[test]
    fn media_over_the_message_limit_is_refused_by_path_and_data_url() {
        let dir = std::env::temp_dir().join(format!("haarness-attach-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        sparse_pdf(&dir.join("scan.pdf"), 21 * MIB);
        let error = admit(vec!["scan.pdf".into()], &dir).unwrap_err();
        assert_eq!(
            error.to_string(),
            "invalid request: scan.pdf is 21 MiB, over the 20 MiB limit for media in one message"
        );
        let encoded = data_url(
            "application/pdf",
            Some("scan.pdf"),
            &std::fs::read(dir.join("scan.pdf")).unwrap(),
        );
        let error = admit(vec![encoded], &dir).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("scan.pdf is 21 MiB, over the 20 MiB limit"),
            "{error}"
        );
        // Just over reads as over, never as the limit itself.
        let audio = dir.join("call.mp3");
        std::fs::File::create(&audio)
            .unwrap()
            .set_len(20 * MIB + 1)
            .unwrap();
        let error = admit(vec!["call.mp3".into()], &dir).unwrap_err();
        assert!(
            error.to_string().contains("call.mp3 is 20.1 MiB"),
            "{error}"
        );
        // At the limit it is admitted.
        sparse_pdf(&dir.join("whole.pdf"), 20 * MIB);
        assert!(admit(vec!["whole.pdf".into()], &dir).is_ok());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn media_that_fits_alone_but_not_together_is_refused() {
        let dir = std::env::temp_dir().join(format!("haarness-attach-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        sparse_pdf(&dir.join("one.pdf"), 12 * MIB);
        sparse_pdf(&dir.join("two.pdf"), 12 * MIB);
        assert!(admit(vec!["one.pdf".into()], &dir).is_ok());
        let error = admit(vec!["one.pdf".into(), "two.pdf".into()], &dir).unwrap_err();
        assert!(
            error.to_string().contains(
                "The attachments come to 24 MiB, over the 20 MiB limit for media in one message"
            ),
            "{error}"
        );
        // One by path and one inline count alike; a text file does not count.
        let inline = data_url("video/mp4", Some("clip.mp4"), &vec![0; 9 * 1024 * 1024]);
        let notes = data_url("text/plain", Some("notes.txt"), b"plain notes\n");
        assert!(admit(vec!["one.pdf".into(), notes.clone()], &dir).is_ok());
        assert!(admit(vec!["one.pdf".into(), inline, notes], &dir).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_file_named_pdf_that_is_not_one_is_taken_for_what_it_holds() {
        let dir = std::env::temp_dir().join(format!("haarness-attach-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        let pointer = "version https://git-lfs.github.com/spec/v1\noid sha256:4d7a\nsize 1048576\n";
        std::fs::write(dir.join("design.pdf"), pointer).unwrap();
        std::fs::write(dir.join("blob.pdf"), b"\0\x01\x02\x03binary").unwrap();
        std::fs::write(dir.join("late.pdf"), b"\r\n%PDF-1.4\n%%EOF\n").unwrap();

        // By path: text is carried as text, binary refused, a header after a
        // little junk is still a PDF.
        let admitted = admit(vec!["design.pdf".into(), "late.pdf".into()], &dir).unwrap();
        assert!(admitted[0].starts_with("data:text/plain;name=design.pdf;base64,"));
        assert_eq!(admitted[1], dir.join("late.pdf").to_string_lossy());
        let (content, media) = for_model("What is this?".into(), admitted, &HashSet::new());
        assert!(content.contains("<attached-file name=\"design.pdf\">\nversion https://git-lfs"));
        assert_eq!(media.len(), 1);
        let error = admit(vec!["blob.pdf".into()], &dir).unwrap_err();
        assert!(
            error.to_string().contains(
                "blob.pdf cannot be attached: it is named as a PDF but does not hold one"
            ),
            "{error}"
        );

        // As a data URL (Desktop, an SDK): the same.
        let typed = data_url("application/pdf", Some("design.pdf"), pointer.as_bytes());
        assert_eq!(
            admit(vec![typed], &dir).unwrap(),
            vec![data_url(
                "text/plain",
                Some("design.pdf"),
                pointer.as_bytes()
            )]
        );
        let png = image(4, 4, image::ImageFormat::Png);
        let admitted = admit(vec![data_url("application/pdf", None, &png)], &dir).unwrap();
        assert!(
            admitted[0].starts_with("data:image/png;base64,"),
            "{}",
            &admitted[0][..40]
        );
        let error = admit(
            vec![data_url("application/pdf", None, b"\0\x01binary")],
            &dir,
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("attachment.pdf cannot be attached"),
            "{error}"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn stored_media_over_the_limit_reaches_the_model_as_a_note() {
        let dir = std::env::temp_dir().join(format!("haarness-attach-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        // Stored before such media was refused, or a file that grew since.
        let big = dir.join("scan.pdf");
        sparse_pdf(&big, 21 * MIB);
        let mut old = b"%PDF-1.7\n".to_vec();
        old.resize(21 * 1024 * 1024, 0);
        let inline = format!(
            "data:application/pdf;name=old.pdf;base64,{}",
            STANDARD.encode(old)
        );
        let small = dir.join("small.pdf");
        sparse_pdf(&small, MIB);
        let big = big.to_string_lossy().into_owned();
        let small = small.to_string_lossy().into_owned();
        let (content, media) = for_model(
            "Read these".into(),
            vec![big.clone(), inline, small.clone()],
            &HashSet::new(),
        );
        assert_eq!(media, vec![small.clone()]);
        assert!(
            content.contains(&format!(
                "[Attachment {big} left out: it would bring this message's media to 21 MiB, over the 20 MiB limit for one message]"
            )),
            "{content}"
        );
        assert!(
            content.contains("[Attachment old.pdf left out: it would bring"),
            "{content}"
        );

        // Two that fit alone: the first is sent, the second named.
        let one = dir.join("one.pdf");
        let two = dir.join("two.pdf");
        sparse_pdf(&one, 12 * MIB);
        sparse_pdf(&two, 12 * MIB);
        let (one, two) = (
            one.to_string_lossy().into_owned(),
            two.to_string_lossy().into_owned(),
        );
        let (content, media) = for_model(
            String::new(),
            vec![one.clone(), two.clone()],
            &HashSet::new(),
        );
        assert_eq!(media, vec![one]);
        assert!(
            content.contains(&format!(
                "[Attachment {two} left out: it would bring this message's media to 24 MiB"
            )),
            "{content}"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn media_a_provider_refused_takes_no_room() {
        let dir = std::env::temp_dir().join(format!("haarness-attach-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        let one = dir.join("one.pdf").to_string_lossy().into_owned();
        let two = dir.join("two.pdf").to_string_lossy().into_owned();
        sparse_pdf(Path::new(&one), 12 * MIB);
        sparse_pdf(Path::new(&two), 12 * MIB);
        // The caller leaves `one` out, so `two` fits.
        let omitted = HashSet::from([fingerprint(&one)]);
        let (content, media) = for_model(String::new(), vec![one.clone(), two.clone()], &omitted);
        assert_eq!(media, vec![one, two]);
        assert!(!content.contains("left out"), "{content}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_stored_pdf_that_no_longer_holds_one_reaches_the_model_as_a_note() {
        let dir = std::env::temp_dir().join(format!("haarness-attach-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        // Admitted as a PDF, then a checkout made it an LFS pointer.
        let design = dir.join("design.pdf");
        std::fs::write(&design, b"%PDF-1.4\n%%EOF\n").unwrap();
        let stored = admit(vec!["design.pdf".into()], &dir).unwrap();
        std::fs::write(&design, "version https://git-lfs.github.com/spec/v1\n").unwrap();
        let mut sources = stored.clone();
        sources.push(data_url("application/pdf", Some("old.pdf"), b"not a pdf"));
        sources.push(data_url("application/pdf", Some("real.pdf"), b"%PDF-1.4\n"));
        let (content, media) = for_model("Read these".into(), sources, &HashSet::new());
        assert_eq!(media.len(), 1);
        assert!(media[0].contains("name=real.pdf"));
        assert!(
            content.contains(&format!(
                "[Attachment {} left out: it does not hold a readable PDF]",
                stored[0]
            )),
            "{content}"
        );
        assert!(content.contains("[Attachment old.pdf left out: it does not hold a readable PDF]"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_message_admitted_again_keeps_what_changed_since_for_the_request_to_name() {
        let dir = std::env::temp_dir().join(format!("haarness-attach-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        sparse_pdf(&dir.join("scan.pdf"), 21 * MIB);
        std::fs::write(dir.join("notes.md"), "# Notes\n").unwrap();
        let kept = admit_each(
            vec![
                "scan.pdf".into(),
                "gone.png".into(),
                dir.join("gone.pdf").to_string_lossy().into_owned(),
                "notes.md".into(),
            ],
            &dir,
        );
        assert_eq!(
            kept[..3],
            [
                dir.join("scan.pdf").to_string_lossy().into_owned(),
                dir.join("gone.png").to_string_lossy().into_owned(),
                dir.join("gone.pdf").to_string_lossy().into_owned(),
            ]
        );
        // What still admits is admitted as before.
        assert!(kept[3].starts_with("data:text/plain;name=notes.md;base64,"));
        let (content, media) = for_model(String::new(), kept, &HashSet::new());
        assert!(media.is_empty());
        assert!(
            content.contains("scan.pdf left out: it would bring"),
            "{content}"
        );
        assert!(
            content.contains("gone.png is no longer available"),
            "{content}"
        );
        assert!(
            content.contains("<attached-file name=\"notes.md\">"),
            "{content}"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_read_stops_one_byte_past_its_limit() {
        let dir = std::env::temp_dir().join(format!("haarness-attach-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("grown.pdf");
        sparse_pdf(&file, 4096);
        assert_eq!(read_at_most(&file, 100).unwrap().len(), 101);
        assert_eq!(read_at_most(&file, 8192).unwrap().len(), 4096);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn sizes_read_in_mebibytes() {
        assert_eq!(mebibytes(20 * MIB), "20 MiB");
        assert_eq!(mebibytes(20 * MIB + 1), "20.1 MiB");
        assert_eq!(mebibytes(24 * MIB + MIB / 2), "24.5 MiB");
    }
}

//! What a person may attach to a message, and how attachments reach a model.
//!
//! Providers take images, PDFs, audio and video. A text file (notes, code,
//! JSON) is read into the conversation as text instead, so a client may
//! attach any file a person would share. Anything else is refused before the
//! message is stored: a stored attachment the provider rejects would fail
//! every later turn of that chat. Images are checked by their bytes and made
//! into ones every provider reads (see [`crate::images`]).

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use base64::{Engine as _, engine::general_purpose::STANDARD};

use crate::{CoreError, CoreResult};

/// The largest text file read into a message.
pub const MAX_TEXT_ATTACHMENT_BYTES: usize = 512 * 1024;

/// The largest image file read for conversion; what is kept is far smaller.
const MAX_IMAGE_SOURCE_BYTES: u64 = 64 * 1024 * 1024;

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
/// changes; anything a model cannot read is refused.
pub fn admit(attachments: Vec<String>, workspace: &Path) -> CoreResult<Vec<String>> {
    attachments
        .into_iter()
        .map(|source| admit_one(source, workspace))
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
            return Ok(source);
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
    if mime_type.is_some() {
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
    Ok(format!(
        "data:text/plain;name={};base64,{}",
        url::form_urlencoded::byte_serialize(name.as_bytes()).collect::<String>(),
        STANDARD.encode(bytes)
    ))
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

//! Local multimodal attachment loading shared by native provider adapters.

use std::path::{Path, PathBuf};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use haarness_core::{CoreError, CoreResult};

/// Shared inline media cap. Adapters with a smaller request envelope (Gemini)
/// retain their own aggregate budget for base64, JSON and conversation text.
pub const MAX_INLINE_MEDIA_BYTES: usize = 20 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct LoadedMedia {
    pub mime_type: String,
    pub base64: String,
    pub filename: String,
}

impl LoadedMedia {
    pub fn data_url(&self) -> String {
        format!("data:{};base64,{}", self.mime_type, self.base64)
    }

    pub fn is_image(&self) -> bool {
        self.mime_type.starts_with("image/")
    }

    pub fn is_pdf(&self) -> bool {
        self.mime_type == "application/pdf"
    }

    pub fn is_audio(&self) -> bool {
        self.mime_type.starts_with("audio/")
    }

    pub fn is_video(&self) -> bool {
        self.mime_type.starts_with("video/")
    }
}

pub fn load_all(sources: &[String]) -> CoreResult<Vec<LoadedMedia>> {
    load_all_with_limit(sources, MAX_INLINE_MEDIA_BYTES)
}

pub(crate) fn load_all_with_limit(sources: &[String], limit: usize) -> CoreResult<Vec<LoadedMedia>> {
    let mut total = 0_usize;
    sources
        .iter()
        .map(|source| {
            let loaded = load(source)?;
            let decoded = (loaded.base64.len() / 4 * 3)
                .saturating_sub(loaded.base64.bytes().rev().take_while(|&byte| byte == b'=').count());
            total = total.saturating_add(decoded);
            if total > limit {
                return Err(CoreError::Provider(format!(
                    "attachments exceed the {} MiB inline media limit",
                    limit / 1024 / 1024
                )));
            }
            Ok(loaded)
        })
        .collect()
}

fn load(source: &str) -> CoreResult<LoadedMedia> {
    if let Some(data) = source.strip_prefix("data:") {
        return load_data_url(data);
    }
    let path = local_path(source)?;
    let mime_type = mime_for_path(&path).ok_or_else(|| {
        CoreError::Provider(format!(
            "unsupported media attachment {}; use a supported image, PDF, audio, or video format",
            path.display()
        ))
    })?;
    let metadata = std::fs::metadata(&path).map_err(|error| {
        CoreError::Provider(format!(
            "cannot read attachment {}: {error}",
            path.display()
        ))
    })?;
    if !metadata.is_file() {
        return Err(CoreError::Provider(format!(
            "attachment is not a file: {}",
            path.display()
        )));
    }
    let size = usize::try_from(metadata.len()).unwrap_or(usize::MAX);
    if size > MAX_INLINE_MEDIA_BYTES {
        return Err(CoreError::Provider(format!(
            "attachment {} is larger than the {} MiB inline media limit",
            path.display(),
            MAX_INLINE_MEDIA_BYTES / 1024 / 1024
        )));
    }
    let bytes = std::fs::read(&path).map_err(|error| {
        CoreError::Provider(format!(
            "cannot read attachment {}: {error}",
            path.display()
        ))
    })?;
    Ok(LoadedMedia {
        mime_type: mime_type.to_owned(),
        base64: STANDARD.encode(bytes),
        filename: path.file_name().map_or_else(
            || "attachment".into(),
            |name| name.to_string_lossy().into_owned(),
        ),
    })
}

fn load_data_url(data: &str) -> CoreResult<LoadedMedia> {
    let (metadata, encoded) = data
        .split_once(',')
        .ok_or_else(|| CoreError::Provider("invalid attachment data URL".into()))?;
    let mut metadata = metadata.split(';');
    let mime_type = metadata.next().unwrap_or_default();
    let parameters = metadata.collect::<Vec<_>>();
    if !parameters.contains(&"base64") {
        return Err(CoreError::Provider(
            "attachment data URL must use base64".into(),
        ));
    }
    if !is_supported_mime(mime_type) {
        return Err(CoreError::Provider(format!(
            "unsupported attachment MIME type {mime_type}"
        )));
    }
    let bytes = STANDARD
        .decode(encoded)
        .map_err(|error| CoreError::Provider(format!("invalid attachment base64: {error}")))?;
    if bytes.len() > MAX_INLINE_MEDIA_BYTES {
        return Err(CoreError::Provider(format!(
            "attachment is larger than the {} MiB inline media limit",
            MAX_INLINE_MEDIA_BYTES / 1024 / 1024
        )));
    }
    Ok(LoadedMedia {
        mime_type: mime_type.to_owned(),
        base64: STANDARD.encode(bytes),
        filename: parameters
            .iter()
            .find_map(|part| part.strip_prefix("name="))
            .and_then(|name| {
                url::form_urlencoded::parse(name.as_bytes())
                    .next()
                    .map(|value| value.0.into_owned())
            })
            .unwrap_or_else(|| default_filename(mime_type).into()),
    })
}

fn local_path(source: &str) -> CoreResult<PathBuf> {
    if source.starts_with("http://") || source.starts_with("https://") {
        return Err(CoreError::Provider(
            "remote attachment URLs are not fetched; attach a local file or data URL".into(),
        ));
    }
    if source.starts_with("file://") {
        return url::Url::parse(source)
            .ok()
            .and_then(|url| url.to_file_path().ok())
            .ok_or_else(|| CoreError::Provider(format!("invalid file URL attachment: {source}")));
    }
    Ok(PathBuf::from(source))
}

pub fn mime_for_path(path: &Path) -> Option<&'static str> {
    haarness_core::attachments::media_mime_for_path(path)
}

fn is_supported_mime(mime_type: &str) -> bool {
    haarness_core::attachments::is_supported_media_mime(mime_type)
}

fn default_filename(mime_type: &str) -> &'static str {
    match mime_type {
        "application/pdf" => "attachment.pdf",
        "audio/mpeg" => "attachment.mp3",
        "audio/wav" => "attachment.wav",
        "audio/flac" => "attachment.flac",
        "audio/mp4" => "attachment.m4a",
        "audio/aac" => "attachment.aac",
        "audio/ogg" => "attachment.ogg",
        "video/mp4" => "attachment.mp4",
        "video/mpeg" => "attachment.mpeg",
        "video/mov" => "attachment.mov",
        "video/avi" => "attachment.avi",
        "video/x-flv" => "attachment.flv",
        "video/mpg" => "attachment.mpg",
        "video/webm" => "attachment.webm",
        "video/wmv" => "attachment.wmv",
        "video/3gpp" => "attachment.3gp",
        "image/jpeg" => "attachment.jpg",
        "image/webp" => "attachment.webp",
        "image/gif" => "attachment.gif",
        "image/heic" => "attachment.heic",
        "image/heif" => "attachment.heif",
        _ => "attachment.png",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_image_data_urls() {
        let media = load("data:image/png;base64,iVBORw0KGgo=").unwrap();
        assert_eq!(media.mime_type, "image/png");
        assert!(media.is_image());
        assert_eq!(media.filename, "attachment.png");

        let named = load("data:image/png;name=screen.png;base64,iVBORw0KGgo=").unwrap();
        assert_eq!(named.mime_type, "image/png");
        assert_eq!(named.filename, "screen.png");
    }

    #[test]
    fn larger_media_reaches_the_adapter_but_provider_budgets_and_aggregate_caps_remain() {
        let large = format!("data:image/png;base64,{}", STANDARD.encode(vec![0; 11 * 1024 * 1024]));
        assert!(load_all(std::slice::from_ref(&large)).is_ok());
        assert!(load_all_with_limit(std::slice::from_ref(&large), 10 * 1024 * 1024).unwrap_err().to_string().contains("10 MiB"));
        assert!(load_all(&[large.clone(), large]).unwrap_err().to_string().contains("20 MiB"));
        let boundary = format!("data:image/png;base64,{}", STANDARD.encode(vec![0; MAX_INLINE_MEDIA_BYTES]));
        assert!(load_all(&[boundary]).is_ok());
    }

    #[test]
    fn identifies_gemini_video_types() {
        assert_eq!(mime_for_path(Path::new("clip.mov")), Some("video/mov"));
        assert_eq!(mime_for_path(Path::new("clip.webm")), Some("video/webm"));
    }

    #[test]
    fn identifies_pdf_and_audio_types() {
        assert_eq!(
            mime_for_path(Path::new("report.pdf")),
            Some("application/pdf")
        );
        assert_eq!(mime_for_path(Path::new("meeting.mp3")), Some("audio/mpeg"));
        assert_eq!(mime_for_path(Path::new("recording.wav")), Some("audio/wav"));

        let pdf = load("data:application/pdf;name=report.pdf;base64,JVBERi0xLjQK").unwrap();
        assert!(pdf.is_pdf());
        assert_eq!(pdf.filename, "report.pdf");

        let audio = load("data:audio/mpeg;name=meeting.mp3;base64,SUQz").unwrap();
        assert!(audio.is_audio());
        assert_eq!(audio.filename, "meeting.mp3");
    }
}

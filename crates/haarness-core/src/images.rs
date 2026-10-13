//! Images as every provider accepts them.
//!
//! Providers read PNG, JPEG, GIF and WebP within size limits, and refuse the
//! whole request otherwise; a stored image they refuse would fail every later
//! turn of its chat. So an image is judged by its bytes, never by its file
//! name or claimed media type: other formats are converted, oversized images
//! are shrunk, and bytes that do not decode are refused before they are kept.

use std::io::Cursor;

use image::{DynamicImage, ImageDecoder as _, ImageFormat, ImageReader, Limits, imageops};

/// The most bytes an image may keep. Anthropic on Amazon Bedrock and Google
/// Cloud takes 5 MB of base64 per image, which is 3.75 MB of image; the
/// other providers take more.
pub const MAX_IMAGE_BYTES: usize = 3_750_000;

/// The longest side an image may keep, in pixels. Anthropic refuses wider
/// images in a request that carries more than 20 of them and reads at most
/// 2576 pixels anyway; OpenAI fits detailed images into 2048.
pub const MAX_IMAGE_SIDE: u32 = 2000;

/// Larger images are refused before they are decoded into memory.
const MAX_SOURCE_SIDE: u32 = 20_000;

/// The image type its leading bytes announce, whatever its name says.
#[must_use]
pub fn sniff(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else if bytes.starts_with(b"BM") && bytes.len() >= 26 {
        Some("image/bmp")
    } else if bytes.starts_with(b"II*\0") || bytes.starts_with(b"MM\0*") {
        Some("image/tiff")
    } else if bytes.len() >= 12 && &bytes[4..8] == b"ftyp" {
        // ISO media: the major brand says which still-image codec it holds.
        match &bytes[8..12] {
            b"heic" | b"heix" | b"heim" | b"heis" | b"hevc" | b"hevx" | b"hevm" | b"hevs" => {
                Some("image/heic")
            }
            b"mif1" | b"msf1" => Some("image/heif"),
            b"avif" | b"avis" => Some("image/avif"),
            _ => None,
        }
    } else {
        None
    }
}

/// Image types every provider reads as they are.
#[must_use]
pub fn is_provider_image(mime_type: &str) -> bool {
    matches!(
        mime_type,
        "image/png" | "image/jpeg" | "image/gif" | "image/webp"
    )
}

/// An image ready to keep and send.
#[derive(Debug, Clone)]
pub struct Prepared {
    pub mime_type: &'static str,
    pub bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// False when the bytes are exactly the ones given.
    pub changed: bool,
}

impl Prepared {
    /// The usual file extension of the prepared bytes.
    #[must_use]
    pub fn extension(&self) -> &'static str {
        match self.mime_type {
            "image/jpeg" => "jpg",
            "image/gif" => "gif",
            "image/webp" => "webp",
            _ => "png",
        }
    }
}

/// Why an image cannot be kept, worded for the person who attached it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The bytes are no image type this build knows.
    NotAnImage,
    /// A known type whose bytes do not decode: damaged or cut short.
    Unreadable(String),
    /// A type that can be neither sent nor converted here.
    Unsupported(&'static str),
    /// Still over the limit after shrinking.
    TooLarge,
}

impl Refusal {
    #[must_use]
    pub fn message(&self, name: &str) -> String {
        match self {
            Self::NotAnImage => format!(
                "{name} is not an image Haarness can read: attach a PNG, JPEG, GIF or WebP file"
            ),
            Self::Unreadable(detail) => format!(
                "{name} cannot be read as an image ({detail}); it may be damaged or incomplete"
            ),
            Self::Unsupported("HEIC") => format!(
                "{name} is a HEIC photo, which Haarness converts only on macOS: export it as JPEG or PNG and attach that"
            ),
            Self::Unsupported(kind) => format!(
                "{name} is a {kind} image, which models do not read: convert it to PNG or JPEG and attach that"
            ),
            Self::TooLarge => format!(
                "{name} is too large to send even after shrinking it: crop it or attach a smaller image"
            ),
        }
    }
}

/// Checks an image and makes it one every provider reads: a known type that
/// decodes, at most [`MAX_IMAGE_SIDE`] pixels on its longest side and
/// [`MAX_IMAGE_BYTES`] long. An image that already is comes back unchanged.
/// Decoding is CPU work; call it off the async executor.
pub fn prepare(bytes: Vec<u8>) -> Result<Prepared, Refusal> {
    prepare_within(bytes, MAX_IMAGE_SIDE, MAX_IMAGE_BYTES)
}

fn prepare_within(bytes: Vec<u8>, max_side: u32, max_bytes: usize) -> Result<Prepared, Refusal> {
    let sniffed = sniff(&bytes).ok_or(Refusal::NotAnImage)?;
    let (bytes, mime_type, converted) = match sniffed {
        "image/heic" | "image/heif" => (
            heic_to_jpeg(&bytes).ok_or(Refusal::Unsupported("HEIC"))?,
            "image/jpeg",
            true,
        ),
        "image/avif" => return Err(Refusal::Unsupported("AVIF")),
        other => (bytes, other, false),
    };
    // Browsers show a PNG whose chunk checksums are wrong; decoders that
    // check them refuse it. Correct checksums make it one both accept.
    let (bytes, repaired) = match repaired_png(&bytes) {
        Some(fixed) => (fixed, true),
        None => (bytes, false),
    };
    let format = ImageFormat::from_mime_type(mime_type).ok_or(Refusal::NotAnImage)?;
    let mut reader = ImageReader::with_format(Cursor::new(&bytes), format);
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SOURCE_SIDE);
    limits.max_image_height = Some(MAX_SOURCE_SIDE);
    reader.limits(limits);
    let mut decoder = reader.into_decoder().map_err(unreadable)?;
    let orientation = decoder.orientation().map_err(unreadable)?;
    // Decoding everything, not just the header, catches a file cut short.
    let mut image = DynamicImage::from_decoder(decoder).map_err(unreadable)?;
    let (width, height) = (image.width(), image.height());
    if width == 0 || height == 0 {
        return Err(Refusal::Unreadable("it has no pixels".into()));
    }
    let keep = !converted
        && is_provider_image(mime_type)
        && bytes.len() <= max_bytes
        && width.max(height) <= max_side
        && !unusual_encoding(&bytes, mime_type);
    if keep {
        return Ok(Prepared {
            mime_type,
            bytes,
            width,
            height,
            changed: repaired,
        });
    }
    // The pixels are re-encoded, so the rotation a camera only recorded in
    // its metadata is applied to them first.
    image.apply_orientation(orientation);
    if image.width().max(image.height()) > max_side {
        image = image.resize(max_side, max_side, imageops::FilterType::Triangle);
    }
    // Lossless sources stay lossless where that fits; photos stay JPEG.
    let lossless = mime_type != "image/jpeg";
    loop {
        if lossless && let Some(png) = encode_png(&image).filter(|png| png.len() <= max_bytes) {
            return Ok(finish(png, "image/png", &image));
        }
        for quality in [85, 70, 55] {
            if let Some(jpeg) = encode_jpeg(&image, quality).filter(|jpeg| jpeg.len() <= max_bytes)
            {
                return Ok(finish(jpeg, "image/jpeg", &image));
            }
        }
        let side = image.width().max(image.height());
        if side <= 256 {
            return Err(Refusal::TooLarge);
        }
        let smaller = side * 3 / 4;
        image = image.resize(smaller, smaller, imageops::FilterType::Triangle);
    }
}

fn finish(bytes: Vec<u8>, mime_type: &'static str, image: &DynamicImage) -> Prepared {
    Prepared {
        mime_type,
        bytes,
        width: image.width(),
        height: image.height(),
        changed: true,
    }
}

fn unreadable(error: image::ImageError) -> Refusal {
    Refusal::Unreadable(match error {
        image::ImageError::Limits(_) => "it is larger than 20,000 pixels on a side".into(),
        error => error.to_string(),
    })
}

/// A copy of a PNG with every chunk checksum corrected, or `None` when they
/// are right already (or the chunks cannot be walked).
fn repaired_png(bytes: &[u8]) -> Option<Vec<u8>> {
    if sniff(bytes) != Some("image/png") {
        return None;
    }
    let mut fixed: Option<Vec<u8>> = None;
    let mut offset = 8;
    while offset + 12 <= bytes.len() {
        let length = usize::try_from(u32::from_be_bytes(
            bytes[offset..offset + 4].try_into().ok()?,
        ))
        .ok()?;
        let end = offset.checked_add(12)?.checked_add(length)?;
        if end > bytes.len() {
            return None;
        }
        let expected = crc32fast::hash(&bytes[offset + 4..end - 4]).to_be_bytes();
        if bytes[end - 4..end] != expected {
            fixed.get_or_insert_with(|| bytes.to_vec())[end - 4..end].copy_from_slice(&expected);
        }
        if &bytes[offset + 4..offset + 8] == b"IEND" {
            break;
        }
        offset = end;
    }
    fixed
}

/// Encodings a provider may refuse though the type is one it reads: PNGs of
/// other than 8 bits per sample, and CMYK JPEGs from print workflows.
fn unusual_encoding(bytes: &[u8], mime_type: &str) -> bool {
    match mime_type {
        // The IHDR chunk always comes first; byte 24 is its bit depth.
        "image/png" => bytes.get(24).is_some_and(|depth| *depth != 8),
        "image/jpeg" => cmyk_jpeg(bytes),
        _ => false,
    }
}

fn cmyk_jpeg(bytes: &[u8]) -> bool {
    let mut offset = 2;
    while offset + 9 < bytes.len() {
        if bytes[offset] != 0xff {
            return false;
        }
        let marker = bytes[offset + 1];
        let length = usize::from(u16::from_be_bytes([bytes[offset + 2], bytes[offset + 3]]));
        if length < 2 {
            return false;
        }
        // A start-of-frame segment names the number of colour components.
        if matches!(marker, 0xc0..=0xc3) {
            return bytes[offset + 9] == 4;
        }
        offset += length + 2;
    }
    false
}

fn encode_png(image: &DynamicImage) -> Option<Vec<u8>> {
    let image = if image.color().has_alpha() {
        DynamicImage::ImageRgba8(image.to_rgba8())
    } else {
        DynamicImage::ImageRgb8(image.to_rgb8())
    };
    let mut bytes = Vec::new();
    image
        .write_with_encoder(image::codecs::png::PngEncoder::new_with_quality(
            &mut bytes,
            image::codecs::png::CompressionType::Default,
            image::codecs::png::FilterType::Adaptive,
        ))
        .ok()?;
    Some(bytes)
}

fn encode_jpeg(image: &DynamicImage, quality: u8) -> Option<Vec<u8>> {
    // JPEG has no transparency: transparent pixels show white, not black.
    let rgba = image.to_rgba8();
    let mut rgb = image::RgbImage::new(rgba.width(), rgba.height());
    for (target, source) in rgb.pixels_mut().zip(rgba.pixels()) {
        let [red, green, blue, alpha] = source.0;
        let blend = |value: u8| {
            let value = u16::from(value) * u16::from(alpha) + 255 * (255 - u16::from(alpha));
            u8::try_from(value / 255).unwrap_or(u8::MAX)
        };
        target.0 = [blend(red), blend(green), blend(blue)];
    }
    let mut bytes = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, quality)
        .encode_image(&rgb)
        .ok()?;
    Some(bytes)
}

/// macOS converts HEIC photos with its own `sips`, so an iPhone photo can be
/// attached as it is; no HEIC decoder ships with Haarness.
#[cfg(target_os = "macos")]
fn heic_to_jpeg(bytes: &[u8]) -> Option<Vec<u8>> {
    use std::os::unix::fs::DirBuilderExt as _;
    use std::process::{Command, Stdio};

    let directory = std::env::temp_dir().join(format!("haarness-heic-{}", uuid::Uuid::now_v7()));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&directory)
        .ok()?;
    let convert = || {
        let input = directory.join("photo.heic");
        let output = directory.join("photo.jpg");
        std::fs::write(&input, bytes).ok()?;
        let mut child = Command::new("/usr/bin/sips")
            .args(["-s", "format", "jpeg", "-s", "formatOptions", "90"])
            .arg(&input)
            .arg("--out")
            .arg(&output)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let status = loop {
            if let Some(status) = child.try_wait().ok()? {
                break status;
            }
            if std::time::Instant::now() > deadline {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        };
        status
            .success()
            .then(|| std::fs::read(&output).ok())
            .flatten()
    };
    let converted = convert();
    let _ = std::fs::remove_dir_all(&directory);
    converted.filter(|jpeg| sniff(jpeg) == Some("image/jpeg"))
}

#[cfg(not(target_os = "macos"))]
fn heic_to_jpeg(_: &[u8]) -> Option<Vec<u8>> {
    None
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn encoded(width: u32, height: u32, format: ImageFormat) -> Vec<u8> {
        let image = image::RgbImage::from_fn(width, height, |x, y| {
            let [red, ..] = x.to_le_bytes();
            let [green, ..] = y.to_le_bytes();
            image::Rgb([red, green, red.wrapping_add(green)])
        });
        let mut bytes = Cursor::new(Vec::new());
        DynamicImage::ImageRgb8(image)
            .write_to(&mut bytes, format)
            .unwrap();
        bytes.into_inner()
    }

    #[test]
    fn the_bytes_decide_the_type() {
        assert_eq!(sniff(&encoded(4, 4, ImageFormat::Png)), Some("image/png"));
        assert_eq!(sniff(&encoded(4, 4, ImageFormat::Jpeg)), Some("image/jpeg"));
        assert_eq!(sniff(&encoded(4, 4, ImageFormat::Bmp)), Some("image/bmp"));
        assert_eq!(sniff(&encoded(4, 4, ImageFormat::Tiff)), Some("image/tiff"));
        assert_eq!(
            sniff(b"\0\0\0\x18ftypheic\0\0\0\0mif1heic"),
            Some("image/heic")
        );
        assert_eq!(sniff(b"\0\0\0\x1cftypavif\0\0\0\0"), Some("image/avif"));
        assert_eq!(sniff(b"plain text"), None);
    }

    #[test]
    fn an_image_providers_read_is_kept_byte_for_byte() {
        let jpeg = encoded(64, 48, ImageFormat::Jpeg);
        let prepared = prepare(jpeg.clone()).unwrap();
        assert!(!prepared.changed);
        assert_eq!(prepared.mime_type, "image/jpeg");
        assert_eq!((prepared.width, prepared.height), (64, 48));
        assert_eq!(prepared.bytes, jpeg);
    }

    #[test]
    fn other_formats_become_png() {
        for format in [ImageFormat::Bmp, ImageFormat::Tiff] {
            let prepared = prepare(encoded(32, 16, format)).unwrap();
            assert!(prepared.changed);
            assert_eq!(prepared.mime_type, "image/png");
            assert_eq!(sniff(&prepared.bytes), Some("image/png"));
            assert_eq!((prepared.width, prepared.height), (32, 16));
        }
    }

    #[test]
    fn oversized_images_shrink_under_both_limits() {
        let wide = prepare(encoded(2400, 60, ImageFormat::Png)).unwrap();
        assert!(wide.changed);
        assert_eq!((wide.width, wide.height), (2000, 50));
        // A noisy image cannot stay PNG within the byte limit: it shrinks
        // and becomes a JPEG until it fits.
        let noisy = image::RgbImage::from_fn(600, 400, |x, y| {
            let seed = x.wrapping_mul(2_654_435_761) ^ y.wrapping_mul(40_503);
            let [red, green, blue, _] = seed.to_le_bytes();
            image::Rgb([red, green, blue])
        });
        let mut png = Cursor::new(Vec::new());
        DynamicImage::ImageRgb8(noisy)
            .write_to(&mut png, ImageFormat::Png)
            .unwrap();
        let png = png.into_inner();
        let limit = 60_000;
        assert!(png.len() > limit);
        let prepared = prepare_within(png, 2000, limit).unwrap();
        assert!(prepared.bytes.len() <= limit, "{}", prepared.bytes.len());
        assert_eq!(prepared.mime_type, "image/jpeg");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_heic_photo_becomes_a_jpeg_on_macos() {
        let directory =
            std::env::temp_dir().join(format!("haarness-heic-test-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("in.png"), encoded(40, 30, ImageFormat::Png)).unwrap();
        let converted = std::process::Command::new("/usr/bin/sips")
            .args(["-s", "format", "heic"])
            .arg(directory.join("in.png"))
            .arg("--out")
            .arg(directory.join("IMG_0001.HEIC"))
            .output()
            .unwrap();
        assert!(converted.status.success(), "{converted:?}");
        let heic = std::fs::read(directory.join("IMG_0001.HEIC")).unwrap();
        std::fs::remove_dir_all(&directory).unwrap();
        assert_eq!(sniff(&heic), Some("image/heic"));
        let prepared = prepare(heic).unwrap();
        assert!(prepared.changed);
        assert_eq!(prepared.mime_type, "image/jpeg");
        assert_eq!((prepared.width, prepared.height), (40, 30));
    }

    #[test]
    fn a_png_with_wrong_checksums_is_corrected_rather_than_refused() {
        let mut png = encoded(3, 2, ImageFormat::Png);
        // The IDAT chunk's checksum, as some hand-made PNGs get it wrong.
        let idat = png.windows(4).position(|window| window == b"IDAT").unwrap() - 4;
        let length = u32::from_be_bytes(png[idat..idat + 4].try_into().unwrap()) as usize;
        png[idat + 8 + length] ^= 0xff;
        let prepared = prepare(png.clone()).unwrap();
        assert!(prepared.changed);
        assert_eq!(prepared.bytes.len(), png.len());
        assert_eq!(repaired_png(&prepared.bytes), None);
        assert_eq!((prepared.width, prepared.height), (3, 2));
    }

    #[test]
    fn damaged_and_unknown_bytes_are_refused_with_a_reason() {
        let mut cut = encoded(64, 64, ImageFormat::Png);
        cut.truncate(cut.len() / 2);
        assert!(matches!(prepare(cut), Err(Refusal::Unreadable(_))));
        assert_eq!(
            prepare(b"not an image".to_vec()).unwrap_err(),
            Refusal::NotAnImage
        );
        assert_eq!(
            prepare(b"\0\0\0\x1cftypavif\0\0\0\0".to_vec()).unwrap_err(),
            Refusal::Unsupported("AVIF")
        );
        assert!(
            Refusal::Unsupported("HEIC")
                .message("IMG_0001.HEIC")
                .contains("export it as JPEG or PNG")
        );
    }
}

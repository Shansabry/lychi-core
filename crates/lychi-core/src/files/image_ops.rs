//! Image operations shared by the `resize` and `convert` handlers — pure,
//! synchronous, and testable without Tauri. Handlers wrap these in
//! `spawn_blocking`.

use std::path::{Path, PathBuf};

use image::ImageFormat;

use super::paths::sibling_output;

/// Hard ceiling on either output dimension. Guards against absurd specs and
/// (later) vision-payload blow-up. Matches the 8000×8000 cap the major vision
/// APIs enforce.
pub const MAX_DIM: u32 = 8000;

/// A parsed resize target.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ResizeSpec {
    /// Exact dimensions (aspect ratio not preserved).
    Exact(u32, u32),
    /// Fixed width, height scaled to preserve aspect.
    Width(u32),
    /// Fixed height, width scaled to preserve aspect.
    Height(u32),
    /// Percentage scale of both dimensions.
    Percent(f32),
}

/// Parse a spec string (`800x600`, `800`, `x600`, `50%`).
pub fn parse_spec(spec: &str) -> Option<ResizeSpec> {
    let spec = spec.trim().to_lowercase();
    if let Some(pct) = spec.strip_suffix('%') {
        let p: f32 = pct.trim().parse().ok()?;
        if p > 0.0 {
            return Some(ResizeSpec::Percent(p / 100.0));
        }
        return None;
    }
    if let Some((w, h)) = spec.split_once('x') {
        let w = w.trim();
        let h = h.trim();
        return match (w.is_empty(), h.is_empty()) {
            (true, false) => h.parse().ok().filter(|n| *n > 0).map(ResizeSpec::Height),
            (false, true) => w.parse().ok().filter(|n| *n > 0).map(ResizeSpec::Width),
            (false, false) => {
                let w: u32 = w.parse().ok().filter(|n| *n > 0)?;
                let h: u32 = h.parse().ok().filter(|n| *n > 0)?;
                Some(ResizeSpec::Exact(w, h))
            }
            (true, true) => None,
        };
    }
    spec.parse().ok().filter(|n| *n > 0).map(ResizeSpec::Width)
}

/// Compute the output dimensions from the source size and spec, clamped to
/// `MAX_DIM`.
pub fn target_dims(spec: ResizeSpec, src_w: u32, src_h: u32) -> (u32, u32) {
    let (w, h) = match spec {
        ResizeSpec::Exact(w, h) => (w.max(1), h.max(1)),
        ResizeSpec::Width(w) => {
            let w = w.max(1);
            let h = ((w as f64 / src_w as f64) * src_h as f64).round() as u32;
            (w, h.max(1))
        }
        ResizeSpec::Height(h) => {
            let h = h.max(1);
            let w = ((h as f64 / src_h as f64) * src_w as f64).round() as u32;
            (w.max(1), h)
        }
        ResizeSpec::Percent(p) => {
            let w = ((src_w as f32) * p).round() as u32;
            let h = ((src_h as f32) * p).round() as u32;
            (w.max(1), h.max(1))
        }
    };
    (w.min(MAX_DIM), h.min(MAX_DIM))
}

/// Load, resize, and save next to the source (`img.jpg` → `img_800x600.jpg`),
/// never overwriting. Returns `(output_path, out_w, out_h)`.
pub fn resize_file(src: &Path, spec: ResizeSpec) -> Result<(PathBuf, u32, u32), String> {
    let img = image::open(src).map_err(|e| format!("Couldn't open image: {e}"))?;
    let (src_w, src_h) = (img.width(), img.height());
    if src_w == 0 || src_h == 0 {
        return Err("Image has zero dimensions".to_string());
    }
    let (w, h) = target_dims(spec, src_w, src_h);
    let resized = img.resize_exact(w, h, image::imageops::FilterType::Lanczos3);

    let out = sibling_output(src, &format!("_{w}x{h}"), None);
    resized
        .save(&out)
        .map_err(|e| format!("Couldn't save resized image: {e}"))?;
    Ok((out, w, h))
}

/// Normalize a user-typed target format (`jpg`/`jpeg`/`JPG` → jpeg) into an
/// `ImageFormat` and the canonical extension to write.
pub fn parse_format(target: &str) -> Option<(ImageFormat, &'static str)> {
    match target.trim().to_ascii_lowercase().as_str() {
        "png" => Some((ImageFormat::Png, "png")),
        "jpg" | "jpeg" => Some((ImageFormat::Jpeg, "jpg")),
        "webp" => Some((ImageFormat::WebP, "webp")),
        "gif" => Some((ImageFormat::Gif, "gif")),
        "bmp" => Some((ImageFormat::Bmp, "bmp")),
        "tiff" | "tif" => Some((ImageFormat::Tiff, "tiff")),
        _ => None,
    }
}

/// Convert an image to `target` format, writing a sibling file with the new
/// extension (`img.png` → `img.webp`). Returns the output path.
pub fn convert_file(src: &Path, target: &str) -> Result<PathBuf, String> {
    let Some((fmt, ext)) = parse_format(target) else {
        return Err(format!(
            "Unsupported format \"{target}\". Try png, jpg, webp, gif, bmp, tiff"
        ));
    };
    let img = image::open(src).map_err(|e| format!("Couldn't open image: {e}"))?;

    let out = sibling_output(src, "", Some(ext));
    // Refuse to clobber the source when it's already that format+name.
    if out == src {
        return Err(format!("Image is already {ext}"));
    }
    // JPEG has no alpha — flatten RGBA onto white to avoid an encoder error.
    let img = if matches!(fmt, ImageFormat::Jpeg) {
        image::DynamicImage::ImageRgb8(img.to_rgb8())
    } else {
        img
    };
    img.save_with_format(&out, fmt)
        .map_err(|e| format!("Couldn't save {ext}: {e}"))?;
    Ok(out)
}

/// Vision payload budget. Images larger than this on their longest edge are
/// downscaled before encoding — keeps the base64 request body small and stays
/// under the vision APIs' per-image pixel limits. 1568 matches Anthropic's
/// recommended long-edge; smaller costs fewer tokens with no quality loss for OCR.
pub const VISION_MAX_EDGE: u32 = 1568;

/// The MIME types every major vision API accepts directly. Anything else we
/// transcode to PNG before sending.
fn vision_native_mime(fmt: ImageFormat) -> Option<&'static str> {
    match fmt {
        ImageFormat::Png => Some("image/png"),
        ImageFormat::Jpeg => Some("image/jpeg"),
        ImageFormat::WebP => Some("image/webp"),
        ImageFormat::Gif => Some("image/gif"),
        _ => None,
    }
}

/// Read an image file and encode it for a vision request: `(media_type, base64)`.
///
/// Downscales to [`VISION_MAX_EDGE`] on the longest side when larger, and
/// transcodes formats the vision APIs don't accept (bmp/tiff/…) to PNG. The
/// returned `data` is raw base64 with NO `data:` URI prefix — the wire encoders
/// add their dialect's wrapper. Pure + synchronous; callers wrap in
/// `spawn_blocking`.
pub fn encode_image_for_vision(src: &Path) -> Result<(String, String), String> {
    use base64::Engine as _;
    use std::io::Cursor;

    let img = image::open(src).map_err(|e| format!("Couldn't open image: {e}"))?;
    let (w, h) = (img.width(), img.height());
    let needs_downscale = w.max(h) > VISION_MAX_EDGE;

    // Pick the output format: keep a natively-supported source format (so a JPEG
    // photo stays a JPEG), otherwise emit PNG. Only known when we've probed the
    // source; fall back to PNG for anything unrecognized.
    let src_fmt = image::ImageReader::open(src)
        .ok()
        .and_then(|r| r.with_guessed_format().ok())
        .and_then(|r| r.format());
    let (out_fmt, mime) = match src_fmt.and_then(|f| vision_native_mime(f).map(|m| (f, m))) {
        Some((f, m)) => (f, m),
        None => (ImageFormat::Png, "image/png"),
    };

    // If no work is needed AND the on-disk bytes are already in a native format,
    // encode the file bytes directly (avoids a needless decode→re-encode round
    // trip that could inflate a well-compressed JPEG).
    if !needs_downscale && src_fmt.is_some_and(|f| vision_native_mime(f).is_some()) {
        let bytes = std::fs::read(src).map_err(|e| format!("Couldn't read image: {e}"))?;
        let data = base64::engine::general_purpose::STANDARD.encode(&bytes);
        return Ok((mime.to_string(), data));
    }

    let img = if needs_downscale {
        img.resize(
            VISION_MAX_EDGE,
            VISION_MAX_EDGE,
            image::imageops::FilterType::Triangle,
        )
    } else {
        img
    };
    // JPEG can't hold alpha — flatten if we're emitting JPEG.
    let img = if matches!(out_fmt, ImageFormat::Jpeg) {
        image::DynamicImage::ImageRgb8(img.to_rgb8())
    } else {
        img
    };

    let mut buf = Cursor::new(Vec::new());
    img.write_to(&mut buf, out_fmt)
        .map_err(|e| format!("Couldn't encode image: {e}"))?;
    let data = base64::engine::general_purpose::STANDARD.encode(buf.into_inner());
    Ok((mime.to_string(), data))
}

/// Longest edge of an attachment-chip thumbnail. Deliberately tiny: the chip
/// renders it at ~20px, and the data URI is embedded in an IPC payload, so a
/// bigger image would cost bytes nobody sees.
pub const THUMB_MAX_EDGE: u32 = 64;

/// Encode a small PNG preview of an image as a `data:` URI, ready to drop
/// straight into an `<img src>`. Always PNG (one format the WebView is certain
/// to render) and always downscaled. Pure + synchronous; callers wrap in
/// `spawn_blocking`.
pub fn encode_thumbnail(src: &Path) -> Result<String, String> {
    use base64::Engine as _;
    use std::io::Cursor;

    let img = image::open(src).map_err(|e| format!("Couldn't open image: {e}"))?;
    let img = img.thumbnail(THUMB_MAX_EDGE, THUMB_MAX_EDGE);
    let mut buf = Cursor::new(Vec::new());
    img.write_to(&mut buf, ImageFormat::Png)
        .map_err(|e| format!("Couldn't encode thumbnail: {e}"))?;
    let data = base64::engine::general_purpose::STANDARD.encode(buf.into_inner());
    Ok(format!("data:image/png;base64,{data}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_specs() {
        assert_eq!(parse_spec("800x600"), Some(ResizeSpec::Exact(800, 600)));
        assert_eq!(parse_spec("800"), Some(ResizeSpec::Width(800)));
        assert_eq!(parse_spec("x600"), Some(ResizeSpec::Height(600)));
        assert_eq!(parse_spec("50%"), Some(ResizeSpec::Percent(0.5)));
        assert_eq!(parse_spec("big"), None);
        assert_eq!(parse_spec("0%"), None);
    }

    #[test]
    fn target_dims_preserve_aspect_and_clamp() {
        assert_eq!(target_dims(ResizeSpec::Width(500), 1000, 500), (500, 250));
        assert_eq!(target_dims(ResizeSpec::Height(250), 1000, 500), (500, 250));
        assert_eq!(target_dims(ResizeSpec::Percent(0.5), 1000, 500), (500, 250));
        // Clamp to MAX_DIM.
        let (w, _) = target_dims(ResizeSpec::Width(99999), 1000, 500);
        assert_eq!(w, MAX_DIM);
    }

    #[test]
    fn parses_formats() {
        assert_eq!(parse_format("JPG").map(|f| f.1), Some("jpg"));
        assert_eq!(parse_format("webp").map(|f| f.1), Some("webp"));
        assert!(parse_format("heic").is_none());
    }

    #[test]
    fn resize_and_convert_roundtrip() {
        // Build a tiny in-memory image, save as PNG, resize, then convert.
        let dir = std::env::temp_dir().join(format!("lychi_imgtest_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("t.png");
        let buf = image::RgbImage::from_pixel(40, 20, image::Rgb([10, 20, 30]));
        image::DynamicImage::ImageRgb8(buf).save(&src).unwrap();

        let (out, w, h) = resize_file(&src, ResizeSpec::Width(20)).unwrap();
        assert_eq!((w, h), (20, 10));
        assert!(out.exists());

        let conv = convert_file(&src, "jpg").unwrap();
        assert_eq!(conv.extension().unwrap(), "jpg");
        assert!(conv.exists());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn vision_encode_native_png_passes_through() {
        use base64::Engine as _;
        let dir = std::env::temp_dir().join(format!("lychi_visiontest_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("small.png");
        let buf = image::RgbImage::from_pixel(32, 16, image::Rgb([1, 2, 3]));
        image::DynamicImage::ImageRgb8(buf).save(&src).unwrap();

        let (mime, data) = encode_image_for_vision(&src).unwrap();
        assert_eq!(mime, "image/png");
        // Round-trips to a valid PNG (magic bytes preserved).
        let raw = base64::engine::general_purpose::STANDARD
            .decode(&data)
            .unwrap();
        assert_eq!(&raw[..8], b"\x89PNG\r\n\x1a\n");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn vision_encode_downscales_oversized() {
        let dir = std::env::temp_dir().join(format!("lychi_visionbig_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Wider than VISION_MAX_EDGE → must be downscaled on the long edge.
        let src = dir.join("big.png");
        let buf = image::RgbImage::from_pixel(VISION_MAX_EDGE + 400, 100, image::Rgb([9, 9, 9]));
        image::DynamicImage::ImageRgb8(buf).save(&src).unwrap();

        let (_mime, data) = encode_image_for_vision(&src).unwrap();
        use base64::Engine as _;
        let raw = base64::engine::general_purpose::STANDARD
            .decode(&data)
            .unwrap();
        let decoded = image::load_from_memory(&raw).unwrap();
        assert!(decoded.width() <= VISION_MAX_EDGE);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn vision_encode_transcodes_bmp_to_png() {
        let dir = std::env::temp_dir().join(format!("lychi_visionbmp_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("t.bmp");
        let buf = image::RgbImage::from_pixel(20, 20, image::Rgb([5, 5, 5]));
        image::DynamicImage::ImageRgb8(buf)
            .save_with_format(&src, ImageFormat::Bmp)
            .unwrap();

        // BMP isn't a native vision format → transcoded to PNG.
        let (mime, _data) = encode_image_for_vision(&src).unwrap();
        assert_eq!(mime, "image/png");

        std::fs::remove_dir_all(&dir).ok();
    }
}

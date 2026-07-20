//! Image encoding, thumbnail generation, and disk I/O for clipboard images.
//!
//! All functions are synchronous — called from the clipboard monitor OS thread.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::Cursor;
use std::path::Path;

use base64::Engine;
use image::codecs::png::PngEncoder;
use image::{ImageEncoder, RgbaImage};

use crate::error::LychiError;

/// Encode raw RGBA pixel data to PNG bytes.
pub fn encode_rgba_to_png(rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>, LychiError> {
    let mut buf = Vec::new();
    let encoder = PngEncoder::new(Cursor::new(&mut buf));
    encoder
        .write_image(rgba, width, height, image::ExtendedColorType::Rgba8)
        .map_err(|e| LychiError::ExecutionFailed(format!("PNG encode failed: {e}")))?;
    Ok(buf)
}

/// Generate a base64-encoded PNG thumbnail that fits within `max_dim x max_dim`.
pub fn generate_thumbnail_b64(
    rgba: &[u8],
    width: u32,
    height: u32,
    max_dim: u32,
) -> Result<String, LychiError> {
    let img = RgbaImage::from_raw(width, height, rgba.to_vec())
        .ok_or_else(|| LychiError::ExecutionFailed("Invalid RGBA buffer dimensions".into()))?;

    let thumb = image::imageops::thumbnail(&img, max_dim, max_dim);

    let mut png_bytes = Vec::new();
    let encoder = PngEncoder::new(Cursor::new(&mut png_bytes));
    encoder
        .write_image(
            thumb.as_raw(),
            thumb.width(),
            thumb.height(),
            image::ExtendedColorType::Rgba8,
        )
        .map_err(|e| LychiError::ExecutionFailed(format!("Thumbnail encode failed: {e}")))?;

    Ok(base64::engine::general_purpose::STANDARD.encode(&png_bytes))
}

/// Save PNG bytes to disk at `clipboard_images_dir()/{uuid}.png`. Returns the absolute path.
pub fn save_png(png_bytes: &[u8], uuid: &str) -> Result<String, LychiError> {
    let dir = crate::paths::clipboard_images_dir();
    std::fs::create_dir_all(&dir).map_err(|e| {
        LychiError::ExecutionFailed(format!("Failed to create clipboard images dir: {e}"))
    })?;

    let path = dir.join(format!("{uuid}.png"));
    std::fs::write(&path, png_bytes).map_err(|e| {
        LychiError::ExecutionFailed(format!("Failed to write clipboard image: {e}"))
    })?;

    Ok(path.to_string_lossy().into_owned())
}

/// Best-effort delete of an image file.
pub fn delete_image(path: &str) {
    let _ = std::fs::remove_file(path);
}

/// Hash image data for dedup. Uses first 4096 bytes of RGBA + dimensions.
pub fn hash_image(rgba: &[u8], width: u32, height: u32) -> u64 {
    let mut hasher = DefaultHasher::new();
    let sample = &rgba[..rgba.len().min(4096)];
    sample.hash(&mut hasher);
    width.hash(&mut hasher);
    height.hash(&mut hasher);
    hasher.finish()
}

/// Clean up orphaned image files that aren't referenced by any redb entry.
pub fn cleanup_orphans(referenced_paths: &[String]) {
    let dir = crate::paths::clipboard_images_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("png") {
            continue;
        }
        let path_str = path.to_string_lossy();
        if !referenced_paths.iter().any(|r| r == path_str.as_ref()) {
            tracing::debug!("[clipboard] removing orphaned image: {}", path_str);
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// Try to read an image from the clipboard via `wl-paste --type image/png`.
/// Returns PNG bytes on success, None on failure.
pub fn wl_paste_image() -> Option<Vec<u8>> {
    use std::process::Command;

    let output = Command::new("wl-paste")
        .args(["--type", "image/png"])
        .output()
        .ok()?;

    if output.status.success() && !output.stdout.is_empty() {
        Some(output.stdout)
    } else {
        None
    }
}

/// Decode PNG bytes to raw RGBA + dimensions. For re-reading saved thumbnails or images.
pub fn decode_png_to_rgba(png_bytes: &[u8]) -> Result<(Vec<u8>, u32, u32), LychiError> {
    let img = image::load_from_memory_with_format(png_bytes, image::ImageFormat::Png)
        .map_err(|e| LychiError::ExecutionFailed(format!("PNG decode failed: {e}")))?;
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    Ok((rgba.into_raw(), w, h))
}

/// Delete all image files in the clipboard images directory.
pub fn clear_all_images() {
    let dir = crate::paths::clipboard_images_dir();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("png") {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
}

/// Check if a file path exists on disk.
pub fn image_exists(path: &str) -> bool {
    Path::new(path).exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_and_decode_png() {
        // 2x2 red image
        let rgba: Vec<u8> = vec![
            255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
        ];
        let png = encode_rgba_to_png(&rgba, 2, 2).unwrap();
        assert!(!png.is_empty());
        // PNG magic bytes
        assert_eq!(&png[..4], &[0x89, 0x50, 0x4E, 0x47]);

        let (decoded, w, h) = decode_png_to_rgba(&png).unwrap();
        assert_eq!(w, 2);
        assert_eq!(h, 2);
        assert_eq!(decoded, rgba);
    }

    #[test]
    fn test_thumbnail_generation() {
        // 100x100 solid blue image
        let mut rgba = Vec::with_capacity(100 * 100 * 4);
        for _ in 0..100 * 100 {
            rgba.extend_from_slice(&[0, 0, 255, 255]);
        }
        let b64 = generate_thumbnail_b64(&rgba, 100, 100, 48).unwrap();
        assert!(!b64.is_empty());

        // Decode the thumbnail to check dimensions
        let thumb_bytes = base64::engine::general_purpose::STANDARD
            .decode(&b64)
            .unwrap();
        let (_, w, h) = decode_png_to_rgba(&thumb_bytes).unwrap();
        assert!(w <= 48);
        assert!(h <= 48);
    }

    #[test]
    fn test_hash_dedup() {
        let rgba1 = vec![0u8; 8192];
        let rgba2 = vec![1u8; 8192];

        let h1 = hash_image(&rgba1, 100, 100);
        let h2 = hash_image(&rgba2, 100, 100);
        let h3 = hash_image(&rgba1, 100, 100);

        assert_eq!(h1, h3); // Same data = same hash
        assert_ne!(h1, h2); // Different data = different hash
    }

    #[test]
    fn test_hash_different_dimensions() {
        let rgba = vec![0u8; 8192];
        let h1 = hash_image(&rgba, 100, 100);
        let h2 = hash_image(&rgba, 200, 50);
        assert_ne!(h1, h2);
    }
}

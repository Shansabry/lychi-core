//! Resolve MPRIS album art to a `data:` URI so the WebView never makes a remote
//! request for it.
//!
//! # Why not just render the URL
//!
//! Spotify (and many players) give `mpris:artUrl` as a remote `https://…` image.
//! Rendering `<img src="https://…">` makes the WebView fetch it directly, which
//! (a) the app's CSP blocks (`img-src` has no `https:`) so the art shows blank,
//! and (b) would leak the user's IP to whatever host a player names — a player
//! is untrusted input. So the fetch happens HERE, in the Rust process we
//! control: download once, hand the WebView an inline `data:` URI (allowed by
//! the CSP's `data:`). A malicious player can make US fetch a URL, but never the
//! WebView, and the result is size-capped and content-type-guarded.
//!
//! `file://` art (local players) is read from disk the same way — also blocked
//! by the CSP as a bare `file://`, also resolved to a `data:` URI here.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use base64::Engine;

use lychi_core::mpris::TrackInfo;

/// Cap on a fetched art image. Cover art is small; anything larger is either not
/// really cover art or an attempt to make us pull a big payload. 5 MiB is far
/// above any real album cover and far below a memory concern.
const MAX_ART_BYTES: usize = 5 * 1024 * 1024;

/// How long to wait on the art host before giving up. Kept short: a missing
/// cover must never stall the media panel.
const ART_TIMEOUT: Duration = Duration::from_secs(4);

/// url → resolved `data:` URI, or `None` when resolution failed (cached too, so
/// a dead/blocked URL isn't re-fetched on every 5s poll). Keyed by the ORIGINAL
/// `mpris:artUrl` string, which is stable per track.
fn cache() -> &'static Mutex<HashMap<String, Option<String>>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Option<String>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Replace `track.art_url` (a remote/`file://` URL) with an inline `data:` URI.
/// A `data:` URI, an already-empty art, or an unresolvable URL is left as-is /
/// cleared so the frontend shows its placeholder rather than a broken image.
pub async fn resolve_track_art(track: &mut TrackInfo) {
    let Some(url) = track.art_url.clone() else {
        return;
    };
    // Already inline (some players do this) — nothing to fetch.
    if url.starts_with("data:") {
        return;
    }
    track.art_url = resolve(&url).await;
}

/// Resolve one art URL to a `data:` URI, using (and populating) the cache.
async fn resolve(url: &str) -> Option<String> {
    if let Some(hit) = cache().lock().ok().and_then(|c| c.get(url).cloned()) {
        return hit; // includes cached `None` (known-unresolvable)
    }
    let resolved = fetch_data_uri(url).await;
    if let Ok(mut c) = cache().lock() {
        // Bound the cache so a long listening session can't grow it without end.
        if c.len() > 256 {
            c.clear();
        }
        c.insert(url.to_string(), resolved.clone());
    }
    resolved
}

/// Fetch/read the art bytes and encode them as a `data:` URI. `None` on any
/// failure (network, too large, not an image, unreadable file).
async fn fetch_data_uri(url: &str) -> Option<String> {
    let (bytes, mime) = if let Some(path) = url.strip_prefix("file://") {
        read_file_art(path).await?
    } else if url.starts_with("http://") || url.starts_with("https://") {
        fetch_remote_art(url).await?
    } else {
        // Unknown scheme — don't guess.
        return None;
    };
    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Some(format!("data:{mime};base64,{b64}"))
}

/// Download remote art, size-capped and content-type-guarded.
async fn fetch_remote_art(url: &str) -> Option<(Vec<u8>, String)> {
    let client = reqwest::Client::builder()
        .timeout(ART_TIMEOUT)
        .build()
        .ok()?;
    let resp = client.get(url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let mime = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.split(';').next().unwrap_or(s).trim().to_string())
        .filter(|m| m.starts_with("image/"))
        .unwrap_or_else(|| "image/jpeg".to_string());
    let bytes = resp.bytes().await.ok()?;
    if bytes.len() > MAX_ART_BYTES {
        return None;
    }
    Some((bytes.to_vec(), mime))
}

/// Read local `file://` art from disk, size-capped, mime guessed from the bytes.
async fn read_file_art(path: &str) -> Option<(Vec<u8>, String)> {
    // A percent-encoded `file://` path (spaces as %20) must be decoded first.
    let decoded = percent_decode(path);
    let meta = tokio::fs::metadata(&decoded).await.ok()?;
    if meta.len() as usize > MAX_ART_BYTES {
        return None;
    }
    let bytes = tokio::fs::read(&decoded).await.ok()?;
    let mime = guess_image_mime(&bytes)?;
    Some((bytes, mime.to_string()))
}

/// Minimal percent-decoding for a `file://` path (enough for spaces and the
/// common escapes MPRIS players emit). Avoids pulling a crate for three cases.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(b) = u8::from_str_radix(&s[i + 1..i + 3], 16)
        {
            out.push(b);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Guess an image mime from magic bytes — the `file://` case has no
/// Content-Type header. Covers the formats cover art actually ships as.
fn guess_image_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        Some("image/png")
    } else if bytes.starts_with(b"GIF8") {
        Some("image/gif")
    } else if bytes.len() > 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guesses_common_image_mimes() {
        assert_eq!(
            guess_image_mime(&[0xFF, 0xD8, 0xFF, 0x00]),
            Some("image/jpeg")
        );
        assert_eq!(guess_image_mime(b"\x89PNG\r\n"), Some("image/png"));
        assert_eq!(guess_image_mime(b"GIF89a"), Some("image/gif"));
        let mut webp = b"RIFF____WEBPVP8 ".to_vec();
        webp.truncate(16);
        assert_eq!(guess_image_mime(&webp), Some("image/webp"));
        assert_eq!(guess_image_mime(b"not an image"), None);
    }

    #[test]
    fn percent_decodes_spaces_and_escapes() {
        assert_eq!(
            percent_decode("/home/u/My%20Music/art.jpg"),
            "/home/u/My Music/art.jpg"
        );
        assert_eq!(percent_decode("/plain/path.png"), "/plain/path.png");
        // A stray % that isn't a valid escape is left intact.
        assert_eq!(percent_decode("/a%zz/b"), "/a%zz/b");
        // An incomplete escape at the very end must not panic (bounds).
        assert_eq!(percent_decode("/path%2"), "/path%2");
        assert_eq!(percent_decode("/path%"), "/path%");
    }
}

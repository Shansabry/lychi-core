use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::clipboard::{ClipboardImageInfo, ClipboardItem};
use crate::db::schema::ClipboardImageMeta;
use crate::error::LychiError;
use crate::filestore::JsonlLog;

/// Maximum text clipboard entries to keep.
const MAX_ENTRIES: usize = 100;
/// Maximum image clipboard entries to keep.
const MAX_IMAGE_ENTRIES: usize = 50;
/// Total on-disk byte budget for clipboard image PNGs. The count cap alone
/// doesn't bound bytes — 50 4K screenshots at ~2.8 MB each is ~140 MB — so this
/// evicts the oldest images (regardless of the count cap) once the directory
/// exceeds the budget. See the backup-size finding that motivated it.
const MAX_IMAGE_BYTES: u64 = 30 * 1024 * 1024;

/// One clipboard entry as stored on disk (JSONL). Carries the id (the redb key
/// in the old store) plus the entry fields. Image *bytes* stay as PNG files in
/// `clipboard_images_dir`; this holds only the path + a tiny thumbnail.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ClipboardRecord {
    id: String,
    text: String,
    created_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    image: Option<ClipboardImageMeta>,
}

/// Clipboard history, an append-only JSONL log (newest last) at
/// [`crate::paths::clipboard_file`].
///
/// Device-local and sensitive (it can hold whatever the user copied), so it
/// lives in a file, not the user-data database — and the file is 0600, enforced
/// by the file store. `clear` unlinks the log and deletes every image, so a
/// cleared history actually reclaims the disk. Image PNGs live in
/// `clipboard_images_dir`; this log carries only their paths + thumbnails.
pub struct ClipboardStore {
    path: PathBuf,
}

impl Default for ClipboardStore {
    fn default() -> Self {
        Self::new()
    }
}

impl ClipboardStore {
    pub fn new() -> Self {
        Self {
            path: crate::paths::clipboard_file(),
        }
    }

    /// Store backed by an explicit file — for tests, so they never touch the real
    /// clipboard log or race each other.
    #[cfg(test)]
    fn with_path(path: PathBuf) -> Self {
        Self { path }
    }

    fn log(&self) -> JsonlLog {
        JsonlLog::new(self.path.clone())
    }

    fn load(&self) -> Vec<ClipboardRecord> {
        self.log().load().unwrap_or_default()
    }

    /// Get clipboard entries, most recent first, up to `limit`.
    pub fn get_entries(&self, limit: usize) -> Result<Vec<ClipboardItem>, LychiError> {
        let items = self
            .load()
            .into_iter()
            .rev() // newest first (file is newest-last)
            .take(limit)
            .map(|r| ClipboardItem {
                id: r.id,
                text: r.text,
                created_at: r.created_at,
                image: r.image.map(|m| ClipboardImageInfo {
                    width: m.width,
                    height: m.height,
                    thumb_b64: m.thumb_b64,
                }),
            })
            .collect();
        Ok(items)
    }

    /// Add a new text clipboard entry. Returns true if actually stored (not a
    /// duplicate of the most recent text entry).
    pub fn push(&self, text: &str) -> Result<bool, LychiError> {
        let text = text.trim();
        if text.is_empty() {
            return Ok(false);
        }
        let mut records = self.load();

        // Duplicate of the most recent text entry? (matches the old newest-row
        // check — a repeated copy of the same text is not re-stored.)
        if let Some(last) = records.last()
            && last.image.is_none()
            && last.text == text
        {
            return Ok(false);
        }

        records.push(ClipboardRecord {
            id: crate::db::new_id(),
            text: text.to_string(),
            created_at: crate::db::now_millis(),
            image: None,
        });

        // Enforce the total text cap (drop oldest). Text records are pruned by
        // count; image pruning (count + byte budget) happens on image push.
        self.prune_text(&mut records);
        self.log().rewrite(&records)?;
        Ok(true)
    }

    /// Add a new image clipboard entry. Returns true if stored.
    pub fn push_image(
        &self,
        path: String,
        width: u32,
        height: u32,
        thumb_b64: String,
    ) -> Result<bool, LychiError> {
        let mut records = self.load();
        records.push(ClipboardRecord {
            id: crate::db::new_id(),
            text: format!("[Image {width}x{height}]"),
            created_at: crate::db::now_millis(),
            image: Some(ClipboardImageMeta {
                path,
                width,
                height,
                thumb_b64,
            }),
        });
        // Prune images by count AND by total on-disk byte budget, deleting the
        // PNG files of anything evicted. Then apply the text cap too.
        self.prune_images(&mut records);
        self.prune_text(&mut records);
        self.log().rewrite(&records)?;
        Ok(true)
    }

    /// Image file path for a specific entry id, if it is an image.
    pub fn get_image_path(&self, id: &str) -> Result<Option<String>, LychiError> {
        Ok(self
            .load()
            .into_iter()
            .find(|r| r.id == id)
            .and_then(|r| r.image.map(|m| m.path)))
    }

    /// Number of clipboard entries.
    pub fn count(&self) -> Result<u64, LychiError> {
        Ok(self.load().len() as u64)
    }

    /// Clear all clipboard history: unlink the log and delete every image file.
    pub fn clear(&self) -> Result<(), LychiError> {
        for path in self.collect_image_paths()? {
            super::image_utils::delete_image(&path);
        }
        self.log().clear()
    }

    /// Every image file path currently referenced (for orphan cleanup / clear).
    pub fn collect_image_paths(&self) -> Result<Vec<String>, LychiError> {
        Ok(self
            .load()
            .into_iter()
            .filter_map(|r| r.image.map(|m| m.path))
            .collect())
    }

    /// Drop the oldest text records so the total stays within `MAX_ENTRIES`.
    /// (`MAX_ENTRIES` bounds ALL entries, matching the old `table.len()` check.)
    fn prune_text(&self, records: &mut Vec<ClipboardRecord>) {
        if records.len() > MAX_ENTRIES {
            let excess = records.len() - MAX_ENTRIES;
            // Deleting oldest entries may drop image records too; free their PNGs.
            for r in records.drain(0..excess) {
                if let Some(img) = r.image {
                    super::image_utils::delete_image(&img.path);
                }
            }
        }
    }

    /// Evict the oldest image records past the count cap OR the byte budget,
    /// deleting their PNG files. Non-image records are untouched.
    fn prune_images(&self, records: &mut Vec<ClipboardRecord>) {
        // Oldest-first list of image record indices + their file sizes.
        let images: Vec<(usize, String, u64)> = records
            .iter()
            .enumerate()
            .filter_map(|(i, r)| {
                r.image.as_ref().map(|m| {
                    let sz = std::fs::metadata(&m.path).map(|md| md.len()).unwrap_or(0);
                    (i, m.path.clone(), sz)
                })
            })
            .collect();

        let total_bytes: u64 = images.iter().map(|(_, _, sz)| sz).sum();
        let count = images.len();

        // How many oldest images must go: enough to satisfy BOTH caps.
        let over_count = count.saturating_sub(MAX_IMAGE_ENTRIES);
        let mut over_bytes = 0usize;
        if total_bytes > MAX_IMAGE_BYTES {
            let mut running = total_bytes;
            for (_, _, sz) in &images {
                if running <= MAX_IMAGE_BYTES {
                    break;
                }
                running -= sz;
                over_bytes += 1;
            }
        }
        let evict = over_count.max(over_bytes);
        if evict == 0 {
            return;
        }

        let doomed_ids: std::collections::HashSet<usize> =
            images.iter().take(evict).map(|(i, _, _)| *i).collect();
        for (_, path, _) in images.iter().take(evict) {
            super::image_utils::delete_image(path);
            tracing::debug!("[clipboard] pruned image entry: {path}");
        }
        let mut i = 0;
        records.retain(|_| {
            let keep = !doomed_ids.contains(&i);
            i += 1;
            keep
        });
    }
}

/// Hash text for quick duplicate comparison in the background monitor.
pub fn hash_text(text: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

/// Background clipboard monitor — polls system clipboard every 500ms and stores new entries.
/// Runs on a dedicated OS thread until `running` is set to false.
/// Automatically recovers from panics (logs and restarts the poll loop).
pub fn run_clipboard_monitor(running: Arc<std::sync::atomic::AtomicBool>) {
    use std::sync::atomic::Ordering;
    tracing::info!("Clipboard monitor started");

    loop {
        if !running.load(Ordering::Relaxed) {
            break;
        }

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            clipboard_monitor_loop(&running);
        }));

        if let Err(_panic) = result {
            tracing::error!(
                "Clipboard monitor panicked — restarting in 1s \
                 (clipboard history may have a gap)"
            );
            std::thread::sleep(std::time::Duration::from_secs(1));
        } else {
            // Loop returned normally — `running` is false, exit cleanly
            break;
        }
    }
    tracing::info!("Clipboard monitor stopped");
}

fn clipboard_monitor_loop(running: &Arc<std::sync::atomic::AtomicBool>) {
    use std::sync::atomic::Ordering;
    let store = ClipboardStore::new();
    let mut last_text_hash: u64 = 0;
    let mut last_image_hash: u64 = 0;
    let is_wayland = std::env::var("WAYLAND_DISPLAY").is_ok();

    // Seed hashes with current clipboard content (don't store what's already
    // there). Use the same readers as the capture pass so the seed matches what
    // we'd capture — otherwise the first Wayland text copy could be missed.
    if let Some((rgba, w, h)) = try_get_image(is_wayland) {
        last_image_hash = super::image_utils::hash_image(&rgba, w, h);
    }
    if let Some(text) = read_clipboard_text(is_wayland) {
        last_text_hash = hash_text(text.trim());
    }

    // Event-driven on Wayland: one persistent `wl-paste --watch` child wakes
    // us per clipboard CHANGE. The 500ms poll fetched and hashed the full
    // clipboard content every tick — with a 4K screenshot sitting there for
    // hours (the normal case), that was a continuous full-RGBA alloc+copy+hash
    // (~66MB/s of churn) plus up to four wl-paste forks per second while
    // fully idle. Event mode does zero work between copies.
    if is_wayland
        && watch_clipboard_events(&store, running, &mut last_text_hash, &mut last_image_hash)
    {
        return; // ran until shutdown in event mode
    }

    // Poll fallback: X11 (no wl-paste), or wl-paste missing/died on Wayland.
    // Text stays at 500ms (cheap fetch); the IMAGE check runs every 4th tick —
    // the full-content fetch is the expensive part and a 2s worst-case capture
    // delay for images is the price of not burning it continuously.
    let mut tick: u32 = 0;
    while running.load(Ordering::Relaxed) {
        std::thread::sleep(std::time::Duration::from_millis(500));
        tick = tick.wrapping_add(1);
        capture_once(
            &store,
            is_wayland,
            /* check_image */ tick.is_multiple_of(4),
            &mut last_text_hash,
            &mut last_image_hash,
        );
    }
}

/// Run one clipboard capture pass: image first (when checked), then text.
/// Shared by the event-driven and polling modes so they cannot drift.
fn capture_once(
    store: &ClipboardStore,
    is_wayland: bool,
    check_image: bool,
    last_text_hash: &mut u64,
    last_image_hash: &mut u64,
) {
    // Try image first — when clipboard has both, image wins
    if check_image && let Some((rgba, width, height)) = try_get_image(is_wayland) {
        let current_hash = super::image_utils::hash_image(&rgba, width, height);
        if current_hash != *last_image_hash {
            *last_image_hash = current_hash;

            // Encode and store
            match process_image_capture(store, &rgba, width, height) {
                Ok(true) => {
                    tracing::debug!("[clipboard] stored image {width}x{height}",);
                }
                Ok(false) => {} // duplicate or error, skip
                Err(e) => {
                    tracing::warn!("[clipboard] image store error: {e}");
                }
            }
            return; // Skip text check — image was the clipboard event
        }
    }

    // Fall through to text. arboard is unreliable for text on Wayland/KDE
    // (returns errors even when text is present), so fall back to `wl-paste`
    // — the same strategy the image path and context detection already use.
    let text = match read_clipboard_text(is_wayland) {
        Some(t) => t,
        None => return,
    };

    let text = text.trim();
    if text.is_empty() {
        return;
    }

    let current_hash = hash_text(text);
    if current_hash == *last_text_hash {
        return;
    }
    // Advance the hash before the privacy check, not after: a skipped
    // secret must not be re-examined on every wake for as long as it sits on
    // the clipboard. We decline to *record* it, we don't keep inspecting it.
    *last_text_hash = current_hash;

    if let Some(reason) = skip_reason(is_wayland) {
        // Log the reason but never the text — a privacy skip that leaks the
        // secret into the log has achieved nothing.
        tracing::debug!("[clipboard] not recorded: {}", reason.as_log_str());
        return;
    }

    if let Err(e) = store.push(text) {
        tracing::warn!("Clipboard store error: {e}");
    }
}

/// Event-driven monitor: block on change notifications from a single
/// persistent `wl-paste --watch` child instead of polling.
///
/// Returns `true` if it ran until shutdown (the caller is done), `false` if
/// event mode is unavailable (spawn failed) or broke mid-run (child died —
/// e.g. compositor restart) and the caller should fall back to polling.
fn watch_clipboard_events(
    store: &ClipboardStore,
    running: &Arc<std::sync::atomic::AtomicBool>,
    last_text_hash: &mut u64,
    last_image_hash: &mut u64,
) -> bool {
    use std::io::BufRead;
    use std::sync::atomic::Ordering;
    use std::sync::mpsc;

    // `--watch echo` runs `echo` (one output line) per clipboard change.
    let mut child = match std::process::Command::new("wl-paste")
        .args(["--watch", "echo"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            tracing::info!("[clipboard] wl-paste --watch unavailable ({e}) — polling instead");
            return false;
        }
    };
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return false;
    };

    // Reader thread: one message per change line; drops the sender on EOF
    // (child died), which surfaces as Disconnected in the recv loop.
    let (tx, rx) = mpsc::channel::<()>();
    std::thread::spawn(move || {
        let reader = std::io::BufReader::new(stdout);
        for line in reader.lines() {
            if line.is_err() || tx.send(()).is_err() {
                break;
            }
        }
    });
    tracing::info!("[clipboard] event-driven (wl-paste --watch)");

    let clean_shutdown = loop {
        match rx.recv_timeout(std::time::Duration::from_millis(500)) {
            Ok(()) => {
                // Coalesce a burst (some apps set the selection several times
                // per copy) into one capture pass.
                while rx.try_recv().is_ok() {}
                capture_once(
                    store,
                    /* is_wayland */ true,
                    /* check_image */ true,
                    last_text_hash,
                    last_image_hash,
                );
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if !running.load(Ordering::Relaxed) {
                    break true;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                tracing::warn!("[clipboard] wl-paste --watch exited — falling back to polling");
                break false;
            }
        }
    };
    let _ = child.kill();
    let _ = child.wait();
    clean_shutdown
}

/// Should this copy be kept out of history? Gathers the two live inputs the
/// pure decider in [`super::sensitive`] needs, and does nothing else — the rule
/// itself lives there and is tested without a compositor.
///
/// Both probes are skipped entirely when the user has turned every exclusion
/// off, so the default-off case costs nothing per copy.
fn skip_reason(is_wayland: bool) -> Option<super::sensitive::SkipReason> {
    use super::sensitive;

    let policy = sensitive::current_policy();
    if !policy.respect_sensitive_hint && policy.excluded_apps.is_empty() {
        return None;
    }

    let offered = if policy.respect_sensitive_hint {
        sensitive::offered_types(is_wayland)
    } else {
        None
    };
    // Only ask who is focused if an exclusion list could actually use it.
    let wm_class = if policy.excluded_apps.is_empty() {
        String::new()
    } else {
        crate::context::snapshot_active_window()
            .map(|w| w.wm_class)
            .unwrap_or_default()
    };

    sensitive::should_skip(&policy, offered.as_deref(), &wm_class)
}

/// Read plain text from the clipboard. Tries arboard first (works on X11 and
/// sometimes Wayland), then falls back to `wl-paste --type text/plain` on
/// Wayland, where arboard is unreliable. Requesting `text/plain` avoids pulling
/// image data when the clipboard holds an image.
fn read_clipboard_text(is_wayland: bool) -> Option<String> {
    if let Ok(mut cb) = arboard::Clipboard::new()
        && let Ok(text) = cb.get_text()
        && !text.trim().is_empty()
    {
        return Some(text);
    }

    if is_wayland
        && let Ok(output) = std::process::Command::new("wl-paste")
            .args(["--no-newline", "--type", "text/plain"])
            .output()
        && output.status.success()
    {
        let text = String::from_utf8_lossy(&output.stdout).to_string();
        if !text.trim().is_empty() {
            return Some(text);
        }
    }

    None
}

/// Try to get an image from the clipboard. Returns (rgba_bytes, width, height).
fn try_get_image(is_wayland: bool) -> Option<(Vec<u8>, u32, u32)> {
    // Try arboard first
    if let Ok(mut cb) = arboard::Clipboard::new()
        && let Ok(img) = cb.get_image()
    {
        let w = img.width as u32;
        let h = img.height as u32;
        if w > 0 && h > 0 {
            return Some((img.bytes.into_owned(), w, h));
        }
    }

    // Wayland fallback: wl-paste --type image/png
    if is_wayland
        && let Some(png_bytes) = super::image_utils::wl_paste_image()
        && let Ok((rgba, w, h)) = super::image_utils::decode_png_to_rgba(&png_bytes)
    {
        return Some((rgba, w, h));
    }

    None
}

/// Encode, thumbnail, save, and push an image clipboard entry.
fn process_image_capture(
    store: &ClipboardStore,
    rgba: &[u8],
    width: u32,
    height: u32,
) -> Result<bool, LychiError> {
    let png_bytes = super::image_utils::encode_rgba_to_png(rgba, width, height)?;
    let thumb_b64 = super::image_utils::generate_thumbnail_b64(rgba, width, height, 48)?;
    let uuid = crate::db::new_id();
    let path = super::image_utils::save_png(&png_bytes, &uuid)?;
    store.push_image(path, width, height, thumb_b64)
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::atomic::{AtomicU64, Ordering};
    fn temp_store() -> ClipboardStore {
        static N: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "lychi_clip_test_{}_{}.jsonl",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_file(&path);
        ClipboardStore::with_path(path)
    }

    #[test]
    fn test_push_and_get() {
        let store = temp_store();

        // Push some entries
        assert!(store.push("hello").unwrap());
        assert!(store.push("world").unwrap());
        assert!(store.push("foo").unwrap());

        // Get entries (most recent first)
        let entries = store.get_entries(10).unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].text, "foo");
        assert_eq!(entries[1].text, "world");
        assert_eq!(entries[2].text, "hello");
        // All text entries — no images
        assert!(entries.iter().all(|e| e.image.is_none()));
    }

    #[test]
    fn test_duplicate_rejection() {
        let store = temp_store();

        assert!(store.push("hello").unwrap());
        assert!(!store.push("hello").unwrap()); // Duplicate
        assert!(store.push("world").unwrap()); // Different
        assert!(!store.push("world").unwrap()); // Duplicate again

        assert_eq!(store.count().unwrap(), 2);
    }

    #[test]
    fn test_empty_text_rejected() {
        let store = temp_store();

        assert!(!store.push("").unwrap());
        assert!(!store.push("   ").unwrap());
        assert_eq!(store.count().unwrap(), 0);
    }

    #[test]
    fn test_clear() {
        let store = temp_store();

        store.push("a").unwrap();
        store.push("b").unwrap();
        assert_eq!(store.count().unwrap(), 2);

        store.clear().unwrap();
        assert_eq!(store.count().unwrap(), 0);
    }

    #[test]
    fn test_limit() {
        let store = temp_store();

        for i in 0..5 {
            store.push(&format!("entry {i}")).unwrap();
        }

        let entries = store.get_entries(3).unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].text, "entry 4"); // Most recent
    }

    #[test]
    fn limited_read_returns_the_newest_not_the_oldest() {
        // The reverse-take rewrite could plausibly return the *first* `limit`
        // rows in key order — i.e. the oldest — while still passing a length
        // check. Assert the actual identities.
        let store = temp_store();
        for i in 0..10 {
            store.push(&format!("entry {i}")).unwrap();
        }

        let entries = store.get_entries(3).unwrap();
        let texts: Vec<&str> = entries.iter().map(|e| e.text.as_str()).collect();
        assert_eq!(texts, vec!["entry 9", "entry 8", "entry 7"]);
    }

    #[test]
    fn limit_larger_than_the_table_returns_everything_newest_first() {
        let store = temp_store();
        store.push("a").unwrap();
        store.push("b").unwrap();

        let entries = store.get_entries(100).unwrap();
        let texts: Vec<&str> = entries.iter().map(|e| e.text.as_str()).collect();
        assert_eq!(texts, vec!["b", "a"]);
    }

    #[test]
    fn test_push_image() {
        let store = temp_store();

        assert!(
            store
                .push_image("/tmp/test.png".into(), 100, 200, "dGh1bWI=".into(),)
                .unwrap()
        );

        let entries = store.get_entries(10).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].text, "[Image 100x200]");
        let img = entries[0].image.as_ref().unwrap();
        assert_eq!(img.width, 100);
        assert_eq!(img.height, 200);
        assert_eq!(img.thumb_b64, "dGh1bWI=");
    }

    #[test]
    fn test_mixed_text_and_image() {
        let store = temp_store();

        store.push("text entry").unwrap();
        store
            .push_image("/tmp/img.png".into(), 50, 50, "thumb".into())
            .unwrap();
        store.push("another text").unwrap();

        let entries = store.get_entries(10).unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].text, "another text");
        assert!(entries[0].image.is_none());
        assert_eq!(entries[1].text, "[Image 50x50]");
        assert!(entries[1].image.is_some());
        assert_eq!(entries[2].text, "text entry");
        assert!(entries[2].image.is_none());
    }
}

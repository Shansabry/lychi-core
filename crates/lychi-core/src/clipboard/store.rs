use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use redb::{Database, ReadableDatabase, ReadableTable, ReadableTableMetadata};

use crate::clipboard::{ClipboardImageInfo, ClipboardItem};
use crate::db::{self, schema::ClipboardEntry};
use crate::error::LychiError;

/// Maximum text clipboard entries to keep.
const MAX_ENTRIES: u64 = 100;
/// Maximum image clipboard entries to keep.
const MAX_IMAGE_ENTRIES: u64 = 50;

#[derive(Default)]
pub struct ClipboardStore;

impl ClipboardStore {
    pub fn new() -> Self {
        Self
    }

    /// Get all clipboard entries, most recent first.
    pub fn get_entries(
        &self,
        db: &Arc<Database>,
        limit: usize,
    ) -> Result<Vec<ClipboardItem>, LychiError> {
        let txn = db.begin_read()?;
        let table = txn.open_table(db::CLIPBOARD)?;
        let mut items = Vec::new();
        // UUID v7 keys are time-ordered, so iterating gives chronological order.
        // Reverse for most-recent-first.
        for result in table.iter()? {
            let (key, val) = result?;
            let entry: ClipboardEntry = postcard::from_bytes(val.value())
                .map_err(|e| LychiError::Database(e.to_string()))?;
            items.push(ClipboardItem {
                id: key.value().to_string(),
                text: entry.text,
                created_at: entry.created_at,
                image: entry.image.map(|m| ClipboardImageInfo {
                    width: m.width,
                    height: m.height,
                    thumb_b64: m.thumb_b64,
                }),
            });
        }
        items.reverse();
        items.truncate(limit);
        Ok(items)
    }

    /// Add a new text clipboard entry. Returns true if it was actually stored (not a duplicate).
    pub fn push(&self, db: &Arc<Database>, text: &str) -> Result<bool, LychiError> {
        let text = text.trim();
        if text.is_empty() {
            return Ok(false);
        }

        // Check if the most recent entry is the same text (avoid duplicates)
        let txn = db.begin_read()?;
        let table = txn.open_table(db::CLIPBOARD)?;
        if let Some(last) = table.iter()?.next_back() {
            let (_key, val) = last?;
            let entry: ClipboardEntry = postcard::from_bytes(val.value())
                .map_err(|e| LychiError::Database(e.to_string()))?;
            if entry.text == text && entry.image.is_none() {
                return Ok(false); // Duplicate of most recent
            }
        }
        drop(table);
        drop(txn);

        // Insert
        let id = db::new_id();
        let entry = ClipboardEntry {
            text: text.to_string(),
            created_at: db::now_millis(),
            image: None,
        };
        let bytes =
            postcard::to_allocvec(&entry).map_err(|e| LychiError::Database(e.to_string()))?;

        let txn = db.begin_write()?;
        {
            let mut table = txn.open_table(db::CLIPBOARD)?;
            table.insert(id.as_str(), bytes.as_slice())?;

            // Prune oldest if over limit
            let len = table.len()?;
            if len > MAX_ENTRIES {
                let to_remove = len - MAX_ENTRIES;
                let mut keys_to_remove = Vec::with_capacity(to_remove as usize);
                for result in table.iter()? {
                    if keys_to_remove.len() >= to_remove as usize {
                        break;
                    }
                    let (key, _) = result?;
                    keys_to_remove.push(key.value().to_string());
                }
                for key in &keys_to_remove {
                    table.remove(key.as_str())?;
                }
            }
        }
        txn.commit()?;

        Ok(true)
    }

    /// Add a new image clipboard entry. Returns true if stored.
    pub fn push_image(
        &self,
        db: &Arc<Database>,
        path: String,
        width: u32,
        height: u32,
        thumb_b64: String,
    ) -> Result<bool, LychiError> {
        let id = db::new_id();
        let entry = ClipboardEntry {
            text: format!("[Image {width}x{height}]"),
            created_at: db::now_millis(),
            image: Some(crate::db::schema::ClipboardImageMeta {
                path,
                width,
                height,
                thumb_b64,
            }),
        };
        let bytes =
            postcard::to_allocvec(&entry).map_err(|e| LychiError::Database(e.to_string()))?;

        let txn = db.begin_write()?;
        {
            let mut table = txn.open_table(db::CLIPBOARD)?;
            table.insert(id.as_str(), bytes.as_slice())?;

            // Prune oldest image entries beyond cap
            prune_image_entries(&mut table, MAX_IMAGE_ENTRIES)?;
        }
        txn.commit()?;

        Ok(true)
    }

    /// Get the image file path for a specific entry by ID.
    pub fn get_image_path(
        &self,
        db: &Arc<Database>,
        id: &str,
    ) -> Result<Option<String>, LychiError> {
        let txn = db.begin_read()?;
        let table = txn.open_table(db::CLIPBOARD)?;
        if let Some(val) = table.get(id)? {
            let entry: ClipboardEntry = postcard::from_bytes(val.value())
                .map_err(|e| LychiError::Database(e.to_string()))?;
            Ok(entry.image.map(|m| m.path))
        } else {
            Ok(None)
        }
    }

    /// Get the number of clipboard entries.
    pub fn count(&self, db: &Arc<Database>) -> Result<u64, LychiError> {
        let txn = db.begin_read()?;
        let table = txn.open_table(db::CLIPBOARD)?;
        Ok(table.len()?)
    }

    /// Clear all clipboard history. Also deletes image files.
    pub fn clear(&self, db: &Arc<Database>) -> Result<(), LychiError> {
        // Collect image paths before clearing
        let image_paths = self.collect_image_paths(db)?;

        let txn = db.begin_write()?;
        {
            let mut table = txn.open_table(db::CLIPBOARD)?;
            let keys: Vec<String> = table
                .iter()?
                .map(|r| r.map(|(k, _)| k.value().to_string()))
                .collect::<Result<_, _>>()?;
            for key in &keys {
                table.remove(key.as_str())?;
            }
        }
        txn.commit()?;

        // Delete image files after commit
        for path in &image_paths {
            super::image_utils::delete_image(path);
        }
        Ok(())
    }

    /// Collect all image file paths from the database (for orphan cleanup / clear).
    pub fn collect_image_paths(&self, db: &Arc<Database>) -> Result<Vec<String>, LychiError> {
        let txn = db.begin_read()?;
        let table = txn.open_table(db::CLIPBOARD)?;
        let mut paths = Vec::new();
        for result in table.iter()? {
            let (_key, val) = result?;
            let entry: ClipboardEntry = postcard::from_bytes(val.value())
                .map_err(|e| LychiError::Database(e.to_string()))?;
            if let Some(img) = entry.image {
                paths.push(img.path);
            }
        }
        Ok(paths)
    }
}

/// Prune oldest image entries beyond `max_images` using a write table. Deletes files.
fn prune_image_entries(
    table: &mut redb::Table<&str, &[u8]>,
    max_images: u64,
) -> Result<(), LychiError> {
    let mut image_entries: Vec<(String, String)> = Vec::new(); // (key, path)
    for result in table.iter()? {
        let (key, val) = result?;
        let entry: ClipboardEntry =
            postcard::from_bytes(val.value()).map_err(|e| LychiError::Database(e.to_string()))?;
        if let Some(img) = entry.image {
            image_entries.push((key.value().to_string(), img.path));
        }
    }

    if image_entries.len() as u64 <= max_images {
        return Ok(());
    }

    let to_remove = image_entries.len() as u64 - max_images;
    for (key, path) in image_entries.iter().take(to_remove as usize) {
        table.remove(key.as_str())?;
        super::image_utils::delete_image(path);
        tracing::debug!("[clipboard] pruned image entry: {path}");
    }

    Ok(())
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
pub fn run_clipboard_monitor(db: Arc<Database>, running: Arc<std::sync::atomic::AtomicBool>) {
    use std::sync::atomic::Ordering;
    tracing::info!("Clipboard monitor started");

    loop {
        if !running.load(Ordering::Relaxed) {
            break;
        }

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            clipboard_monitor_loop(&db, &running);
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

fn clipboard_monitor_loop(db: &Arc<Database>, running: &Arc<std::sync::atomic::AtomicBool>) {
    use std::sync::atomic::Ordering;
    let store = ClipboardStore::new();
    let mut last_text_hash: u64 = 0;
    let mut last_image_hash: u64 = 0;
    let is_wayland = std::env::var("WAYLAND_DISPLAY").is_ok();

    // Seed hashes with current clipboard content (don't store what's already
    // there). Use the same readers as the poll loop so the seed matches what
    // we'd capture — otherwise the first Wayland text copy could be missed.
    if let Some((rgba, w, h)) = try_get_image(is_wayland) {
        last_image_hash = super::image_utils::hash_image(&rgba, w, h);
    }
    if let Some(text) = read_clipboard_text(is_wayland) {
        last_text_hash = hash_text(text.trim());
    }

    while running.load(Ordering::Relaxed) {
        std::thread::sleep(std::time::Duration::from_millis(500));

        // Try image first — when clipboard has both, image wins
        if let Some((rgba, width, height)) = try_get_image(is_wayland) {
            let current_hash = super::image_utils::hash_image(&rgba, width, height);
            if current_hash != last_image_hash {
                last_image_hash = current_hash;

                // Encode and store
                match process_image_capture(&store, db, &rgba, width, height) {
                    Ok(true) => {
                        tracing::debug!("[clipboard] stored image {width}x{height}",);
                    }
                    Ok(false) => {} // duplicate or error, skip
                    Err(e) => {
                        tracing::warn!("[clipboard] image store error: {e}");
                    }
                }
                continue; // Skip text check — image was the clipboard event
            }
        }

        // Fall through to text. arboard is unreliable for text on Wayland/KDE
        // (returns errors even when text is present), so fall back to `wl-paste`
        // — the same strategy the image path and context detection already use.
        let text = match read_clipboard_text(is_wayland) {
            Some(t) => t,
            None => continue,
        };

        let text = text.trim();
        if text.is_empty() {
            continue;
        }

        let current_hash = hash_text(text);
        if current_hash == last_text_hash {
            continue;
        }
        last_text_hash = current_hash;

        if let Err(e) = store.push(db, text) {
            tracing::warn!("Clipboard store error: {e}");
        }
    }
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
    db: &Arc<Database>,
    rgba: &[u8],
    width: u32,
    height: u32,
) -> Result<bool, LychiError> {
    let png_bytes = super::image_utils::encode_rgba_to_png(rgba, width, height)?;
    let thumb_b64 = super::image_utils::generate_thumbnail_b64(rgba, width, height, 48)?;
    let uuid = crate::db::new_id();
    let path = super::image_utils::save_png(&png_bytes, &uuid)?;
    store.push_image(db, path, width, height, thumb_b64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_push_and_get() {
        let db = crate::db::open_test_database();
        let store = ClipboardStore::new();

        // Push some entries
        assert!(store.push(&db, "hello").unwrap());
        assert!(store.push(&db, "world").unwrap());
        assert!(store.push(&db, "foo").unwrap());

        // Get entries (most recent first)
        let entries = store.get_entries(&db, 10).unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].text, "foo");
        assert_eq!(entries[1].text, "world");
        assert_eq!(entries[2].text, "hello");
        // All text entries — no images
        assert!(entries.iter().all(|e| e.image.is_none()));
    }

    #[test]
    fn test_duplicate_rejection() {
        let db = crate::db::open_test_database();
        let store = ClipboardStore::new();

        assert!(store.push(&db, "hello").unwrap());
        assert!(!store.push(&db, "hello").unwrap()); // Duplicate
        assert!(store.push(&db, "world").unwrap()); // Different
        assert!(!store.push(&db, "world").unwrap()); // Duplicate again

        assert_eq!(store.count(&db).unwrap(), 2);
    }

    #[test]
    fn test_empty_text_rejected() {
        let db = crate::db::open_test_database();
        let store = ClipboardStore::new();

        assert!(!store.push(&db, "").unwrap());
        assert!(!store.push(&db, "   ").unwrap());
        assert_eq!(store.count(&db).unwrap(), 0);
    }

    #[test]
    fn test_clear() {
        let db = crate::db::open_test_database();
        let store = ClipboardStore::new();

        store.push(&db, "a").unwrap();
        store.push(&db, "b").unwrap();
        assert_eq!(store.count(&db).unwrap(), 2);

        store.clear(&db).unwrap();
        assert_eq!(store.count(&db).unwrap(), 0);
    }

    #[test]
    fn test_limit() {
        let db = crate::db::open_test_database();
        let store = ClipboardStore::new();

        for i in 0..5 {
            store.push(&db, &format!("entry {i}")).unwrap();
        }

        let entries = store.get_entries(&db, 3).unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].text, "entry 4"); // Most recent
    }

    #[test]
    fn test_push_image() {
        let db = crate::db::open_test_database();
        let store = ClipboardStore::new();

        assert!(
            store
                .push_image(&db, "/tmp/test.png".into(), 100, 200, "dGh1bWI=".into(),)
                .unwrap()
        );

        let entries = store.get_entries(&db, 10).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].text, "[Image 100x200]");
        let img = entries[0].image.as_ref().unwrap();
        assert_eq!(img.width, 100);
        assert_eq!(img.height, 200);
        assert_eq!(img.thumb_b64, "dGh1bWI=");
    }

    #[test]
    fn test_mixed_text_and_image() {
        let db = crate::db::open_test_database();
        let store = ClipboardStore::new();

        store.push(&db, "text entry").unwrap();
        store
            .push_image(&db, "/tmp/img.png".into(), 50, 50, "thumb".into())
            .unwrap();
        store.push(&db, "another text").unwrap();

        let entries = store.get_entries(&db, 10).unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].text, "another text");
        assert!(entries[0].image.is_none());
        assert_eq!(entries[1].text, "[Image 50x50]");
        assert!(entries[1].image.is_some());
        assert_eq!(entries[2].text, "text entry");
        assert!(entries[2].image.is_none());
    }
}

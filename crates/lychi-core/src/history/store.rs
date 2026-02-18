use std::fs;
use std::path::PathBuf;

use crate::error::LychiError;

pub struct HistoryStore {
    entries: Vec<String>,
    path: PathBuf,
    max_entries: usize,
    deduplicate: bool,
}

impl HistoryStore {
    /// Create an empty in-memory history store (no disk persistence until save succeeds).
    pub fn empty(path: PathBuf, max_entries: usize, deduplicate: bool) -> Self {
        Self {
            entries: Vec::new(),
            path,
            max_entries,
            deduplicate,
        }
    }

    pub fn load_or_create(
        path: &PathBuf,
        max_entries: usize,
        deduplicate: bool,
    ) -> Result<Self, LychiError> {
        let entries = if path.exists() {
            let content = fs::read_to_string(path)?;
            serde_json::from_str(&content).unwrap_or_else(|e| {
                tracing::warn!(
                    "Corrupt history file {}: {e} — starting fresh",
                    path.display()
                );
                Vec::new()
            })
        } else {
            Vec::new()
        };

        Ok(Self {
            entries,
            path: path.clone(),
            max_entries,
            deduplicate,
        })
    }

    pub fn push(&mut self, entry: &str) {
        let entry = entry.trim().to_string();
        if entry.is_empty() {
            return;
        }

        if self.deduplicate {
            self.entries.retain(|e| e != &entry);
        }

        self.entries.push(entry);

        if self.entries.len() > self.max_entries {
            let excess = self.entries.len() - self.max_entries;
            self.entries.drain(..excess);
        }

        if let Err(e) = self.save() {
            tracing::error!("Failed to save history: {e}");
        }
    }

    pub fn entries(&self) -> &[String] {
        &self.entries
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        if let Err(e) = self.save() {
            tracing::error!("Failed to save history after clear: {e}");
        }
    }

    fn save(&self) -> Result<(), LychiError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(&self.entries)?;
        fs::write(&self.path, json)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    use std::sync::atomic::{AtomicU64, Ordering};

    fn temp_path() -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        env::temp_dir().join(format!(
            "lychi-test-history-{}-{id}.json",
            std::process::id()
        ))
    }

    #[test]
    fn push_and_retrieve() {
        let path = temp_path();
        let mut store = HistoryStore::load_or_create(&path, 500, true).unwrap();
        store.push("web rust");
        store.push("open firefox");
        assert_eq!(store.entries(), &["web rust", "open firefox"]);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn deduplication() {
        let path = temp_path();
        let mut store = HistoryStore::load_or_create(&path, 500, true).unwrap();
        store.push("web rust");
        store.push("open firefox");
        store.push("web rust");
        assert_eq!(store.entries(), &["open firefox", "web rust"]);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn max_entries_enforced() {
        let path = temp_path();
        let mut store = HistoryStore::load_or_create(&path, 3, false).unwrap();
        store.push("a");
        store.push("b");
        store.push("c");
        store.push("d");
        assert_eq!(store.entries(), &["b", "c", "d"]);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn persistence() {
        let path = temp_path();
        {
            let mut store = HistoryStore::load_or_create(&path, 500, true).unwrap();
            store.push("web rust");
            store.push("open firefox");
        }
        {
            let store = HistoryStore::load_or_create(&path, 500, true).unwrap();
            assert_eq!(store.entries(), &["web rust", "open firefox"]);
        }
        let _ = fs::remove_file(&path);
    }
}

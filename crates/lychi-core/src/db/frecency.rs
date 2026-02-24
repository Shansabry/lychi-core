use std::collections::HashMap;
use std::sync::Arc;

use redb::{Database, ReadableDatabase, ReadableTable};
use serde::{Deserialize, Serialize};

use crate::error::LychiError;

use super::FRECENCY;

/// Frecency entry: tracks access frequency and recency for a single item.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrecencyEntry {
    /// Total number of times this item was accessed.
    pub count: u32,
    /// Timestamps of last N accesses (ring buffer, keep last 10).
    /// Milliseconds since UNIX epoch.
    pub recent_timestamps: Vec<u64>,
}

const MAX_RECENT: usize = 10;

impl FrecencyEntry {
    fn new(now_ms: u64) -> Self {
        Self {
            count: 1,
            recent_timestamps: vec![now_ms],
        }
    }

    /// Record a new access.
    pub fn record_access(&mut self, now_ms: u64) {
        self.count += 1;
        self.recent_timestamps.push(now_ms);
        if self.recent_timestamps.len() > MAX_RECENT {
            self.recent_timestamps.remove(0);
        }
    }

    /// Calculate frecency score (0.0 to 1.0, normalized).
    ///
    /// Recency weights:
    /// - Last hour:  1.0
    /// - Last day:   0.7
    /// - Last week:  0.4
    /// - Older:      0.1
    pub fn score(&self, now_ms: u64) -> f64 {
        let mut total = 0.0;
        for &ts in &self.recent_timestamps {
            let age_hours = (now_ms.saturating_sub(ts)) as f64 / 3_600_000.0;
            let weight = if age_hours < 1.0 {
                1.0
            } else if age_hours < 24.0 {
                0.7
            } else if age_hours < 168.0 {
                // 7 days
                0.4
            } else {
                0.1
            };
            total += weight;
        }
        // Normalize: max possible = MAX_RECENT * 1.0
        (total / MAX_RECENT as f64).min(1.0)
    }
}

/// Record an access for a given key.
pub fn record(db: &Arc<Database>, key: &str) -> Result<(), LychiError> {
    let now_ms = super::now_millis();
    let txn = db.begin_write()?;
    {
        let mut table = txn.open_table(FRECENCY)?;

        let entry = match table.get(key)? {
            Some(existing) => {
                let mut entry: FrecencyEntry =
                    postcard::from_bytes(existing.value()).unwrap_or(FrecencyEntry::new(now_ms));
                entry.record_access(now_ms);
                entry
            }
            None => FrecencyEntry::new(now_ms),
        };

        let bytes = postcard::to_allocvec(&entry)
            .map_err(|e| LychiError::Database(format!("frecency serialize: {e}")))?;
        table.insert(key, bytes.as_slice())?;
    }
    txn.commit()?;
    Ok(())
}

/// Get frecency scores for all tracked items.
/// Returns a map of key -> score (0.0 to 1.0).
pub fn get_scores(db: &Arc<Database>) -> HashMap<String, f64> {
    let now_ms = super::now_millis();
    let mut scores = HashMap::new();

    let Ok(txn) = db.begin_read() else {
        return scores;
    };
    let Ok(table) = txn.open_table(FRECENCY) else {
        return scores;
    };
    let Ok(iter) = table.iter() else {
        return scores;
    };

    for item in iter.flatten() {
        let (key, value) = item;
        let Ok(entry) = postcard::from_bytes::<FrecencyEntry>(value.value()) else {
            continue;
        };
        let score = entry.score(now_ms);
        if score > 0.0 {
            scores.insert(key.value().to_string(), score);
        }
    }

    scores
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_frecency_score_recent() {
        let now = 1_700_000_000_000u64;
        let entry = FrecencyEntry {
            count: 5,
            recent_timestamps: vec![
                now - 1_000,      // 1 second ago
                now - 60_000,     // 1 minute ago
                now - 1_800_000,  // 30 minutes ago
                now - 7_200_000,  // 2 hours ago
                now - 86_400_000, // 1 day ago
            ],
        };
        let score = entry.score(now);
        // 3 within hour (3.0) + 1 within day (0.7) + 1 within week (0.4) = 4.1 / 10 = 0.41
        assert!(score > 0.3 && score < 0.5, "score was {score}");
    }

    #[test]
    fn test_frecency_score_empty() {
        let entry = FrecencyEntry {
            count: 0,
            recent_timestamps: vec![],
        };
        assert_eq!(entry.score(1_700_000_000_000), 0.0);
    }

    #[test]
    fn test_record_and_get() {
        let db = crate::db::open_test_database();
        record(&db, "firefox").unwrap();
        record(&db, "firefox").unwrap();
        record(&db, "terminal").unwrap();

        let scores = get_scores(&db);
        assert!(scores.contains_key("firefox"));
        assert!(scores.contains_key("terminal"));
        assert!(scores["firefox"] > scores["terminal"]);
    }
}

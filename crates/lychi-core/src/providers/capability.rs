//! What the selected model can actually do — learned, never hardcoded.
//!
//! Lychi's model field is deliberately free-form (any id the endpoint accepts),
//! so there is no fixed list to consult and no provider table to maintain. This
//! module answers two questions about a `<provider>/<model>` pair:
//!
//!   1. **Can it see images?** ([`Vision`]) — from provider metadata when the
//!      endpoint reports modalities, otherwise learned from an observed rejection
//!      (an unknown model fails once, then never again).
//!   2. **Roughly how capable is it?** ([`Estimate`]) — a coarse tier the "AI
//!      potential meter" fills in, computed on a model/mode change. Optional:
//!      absent until the meter has scored the model.
//!
//! # Where this lives
//!
//! This is derived, machine-learned data — NOT user-authored content — so it
//! lives in a file (`filestore`), not the user-data database. The store is an
//! append-only JSONL log keyed by `<provider>/<model>`: a `record`/`upsert`
//! appends a fresh line, and reads fold the log so the LAST line for a key wins.
//! Because there is one row per model the user actually touches, the log stays
//! tiny; it is compacted (rewritten to one line per key) opportunistically.
//!
//! `Unknown` is a real answer for vision: it means "no evidence either way", and
//! callers must treat it as *permission to try*, not a refusal — blocking on
//! absent evidence would make every newly released model unusable.

use std::path::PathBuf;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::error::LychiError;
use crate::filestore::JsonlLog;

/// Whether a model accepts image input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
#[serde(rename_all = "lowercase")]
pub enum Vision {
    /// Confirmed to accept images (provider metadata said so).
    Supported,
    /// Confirmed NOT to accept images (the provider rejected an image request).
    Unsupported,
    /// No evidence yet. Callers should ALLOW the attempt — this is what makes a
    /// newly released vision model work on day one without a Lychi update.
    Unknown,
}

/// A coarse capability estimate for the "AI potential meter". Filled in by the
/// meter on a model/mode change; `None` on a record until then. Ordered weak →
/// strong. The scoring that produces it lives in `providers::potential`; the
/// type lives here because this is where it is stored.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, specta::Type,
)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    /// Small local models (roughly < 7B). Expect simpler reasoning and misses on
    /// complex commands — the tier that surfaces the "experimental" caveat.
    Basic,
    /// Mid-size models (~7–30B) or a typical BYO setup. Handles most tasks.
    Capable,
    /// Frontier hosted models, or large local models (~30B+). No caveat.
    Full,
}

impl Tier {
    /// One notch weaker, saturating at `Basic` — penalises heavy quantization
    /// without underflowing.
    pub fn demote(self) -> Self {
        match self {
            Tier::Full => Tier::Capable,
            Tier::Capable => Tier::Basic,
            Tier::Basic => Tier::Basic,
        }
    }

    /// The label shown in the meter.
    pub fn label(self) -> &'static str {
        match self {
            Tier::Basic => "Basic",
            Tier::Capable => "Capable",
            Tier::Full => "Full",
        }
    }

    /// Fraction of the meter to fill (1/3, 2/3, 3/3).
    pub fn fill(self) -> f32 {
        match self {
            Tier::Basic => 1.0 / 3.0,
            Tier::Capable => 2.0 / 3.0,
            Tier::Full => 1.0,
        }
    }

    /// Whether this tier warrants the "expect simpler reasoning" caveat banner.
    pub fn is_low(self) -> bool {
        self == Tier::Basic
    }
}

/// The stored estimate plus the display signals it was based on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, specta::Type)]
pub struct Estimate {
    pub tier: Tier,
    /// e.g. "3B", "" when unknown. Display only.
    pub params_label: String,
    /// e.g. "Q4", "" when unknown. Display only.
    pub quant_label: String,
}

/// A learned capability record for one model, as stored on disk (one JSON line).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelCapability {
    /// `<provider>/<model>` — the log key. Stored in the record so a folded read
    /// can group by it without an out-of-band key.
    pub key: String,
    pub vision: Vision,
    /// True when the vision verdict came from provider metadata rather than an
    /// observed failure. Metadata wins if the two ever disagree.
    pub from_metadata: bool,
    /// The capability-meter estimate, once computed. Carried across records for a
    /// key so recording a vision verdict doesn't wipe a prior estimate (and vice
    /// versa) — see [`upsert`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimate: Option<Estimate>,
    pub updated_at: u64,
}

/// The process-wide capability store path.
///
/// The provider factory is a pure config→provider function called from several
/// contexts (startup, a settings change, a reactor thread), none of which carry
/// a store handle. The app registers the path once at startup and everything
/// reads it here. Unset (tests, or before init) means nothing is learned/stored.
static STORE: OnceLock<PathBuf> = OnceLock::new();

/// Register the capability store file. Called once, at app startup. Later calls
/// are ignored — the path never changes during a run.
pub fn init_store(path: PathBuf) {
    let _ = STORE.set(path);
}

/// The registered store path, if any.
pub fn store() -> Option<&'static PathBuf> {
    STORE.get()
}

/// The JSONL log at the registered path, if a store is registered.
fn log() -> Option<JsonlLog> {
    STORE.get().map(JsonlLog::new)
}

/// The storage key. Namespaced by provider because the same model id can mean
/// different things across endpoints (a proxy may alias `gpt-4o` to anything).
pub fn key(provider: &str, model: &str) -> String {
    format!("{}/{}", provider.trim().to_lowercase(), model.trim())
}

/// Fold the append log into the latest record per key (last line wins).
fn latest_by_key(log: &JsonlLog) -> std::collections::HashMap<String, ModelCapability> {
    let mut map = std::collections::HashMap::new();
    if let Ok(records) = log.load::<ModelCapability>() {
        for rec in records {
            map.insert(rec.key.clone(), rec);
        }
    }
    map
}

/// The current record for a key, if any (from the folded log).
fn current(log: &JsonlLog, k: &str) -> Option<ModelCapability> {
    latest_by_key(log).remove(k)
}

/// Look up what we know about a model's vision support. `Unknown` when we've
/// never seen it — which callers must treat as "go ahead and try".
pub fn get_vision(provider: &str, model: &str) -> Vision {
    if model.trim().is_empty() {
        return Vision::Unknown;
    }
    let Some(log) = log() else {
        return Vision::Unknown;
    };
    current(&log, &key(provider, model))
        .map(|c| c.vision)
        .unwrap_or(Vision::Unknown)
}

/// The stored capability estimate for a model, if the meter has computed one.
pub fn get_estimate(provider: &str, model: &str) -> Option<Estimate> {
    let log = log()?;
    current(&log, &key(provider, model)).and_then(|c| c.estimate)
}

/// Record a learned vision verdict. Metadata is authoritative: an observed
/// failure never overwrites a metadata verdict, because a 400 can have causes
/// other than vision while a modality declaration is a direct statement.
/// Preserves any existing [`Estimate`] on the key.
pub fn record(
    provider: &str,
    model: &str,
    vision: Vision,
    from_metadata: bool,
) -> Result<(), LychiError> {
    if model.trim().is_empty() || vision == Vision::Unknown {
        return Ok(()); // nothing worth storing
    }
    let Some(log) = log() else {
        return Ok(()); // no store registered (tests / pre-init)
    };
    let k = key(provider, model);
    let existing = current(&log, &k);

    if !from_metadata && matches!(&existing, Some(c) if c.from_metadata) {
        return Ok(()); // don't let an observation override metadata
    }

    let entry = ModelCapability {
        vision,
        from_metadata,
        // Carry a prior estimate forward — a vision update must not wipe it.
        estimate: existing.and_then(|c| c.estimate),
        updated_at: crate::db::now_millis(),
        key: k,
    };
    append_and_maybe_compact(&log, entry)
}

/// Record/update the capability estimate for a model (from the potential meter),
/// preserving any known vision verdict.
pub fn record_estimate(provider: &str, model: &str, estimate: Estimate) -> Result<(), LychiError> {
    if model.trim().is_empty() {
        return Ok(());
    }
    let Some(log) = log() else {
        return Ok(());
    };
    let k = key(provider, model);
    let existing = current(&log, &k);

    let entry = ModelCapability {
        vision: existing
            .as_ref()
            .map(|c| c.vision)
            .unwrap_or(Vision::Unknown),
        from_metadata: existing.as_ref().map(|c| c.from_metadata).unwrap_or(false),
        estimate: Some(estimate),
        updated_at: crate::db::now_millis(),
        key: k,
    };
    append_and_maybe_compact(&log, entry)
}

/// Append one record. When the append log has grown to more than a few lines per
/// live key (stale duplicates from repeated upserts), rewrite it compacted to one
/// line per key. Keeps the file from growing without bound while making the
/// common path a cheap append.
fn append_and_maybe_compact(log: &JsonlLog, entry: ModelCapability) -> Result<(), LychiError> {
    log.append(&entry)?;
    // Compaction heuristic: if raw lines exceed 4× the number of distinct keys,
    // fold and rewrite. Cheap because the whole thing is tiny (one row per model
    // the user has touched).
    let raw = log.approx_len().unwrap_or(0);
    if raw > 8 {
        let folded = latest_by_key(log);
        if raw > folded.len().saturating_mul(4).max(4) {
            let compacted: Vec<ModelCapability> = folded.into_values().collect();
            log.rewrite(&compacted)?;
        }
    }
    Ok(())
}

/// Extract vision support from an OpenAI-compatible `/models` entry.
///
/// Only OpenRouter-style responses carry modality data; a bare `{id, object}`
/// entry (OpenAI, Groq) yields `Unknown` rather than a guess. Also accepts the
/// flat `modality: "text+image->text"` string some versions return.
pub fn vision_from_model_json(entry: &serde_json::Value) -> Vision {
    let arch = &entry["architecture"];

    if let Some(mods) = arch["input_modalities"].as_array() {
        let has_image = mods.iter().any(|m| m.as_str() == Some("image"));
        return if has_image {
            Vision::Supported
        } else {
            Vision::Unsupported
        };
    }

    // Older/flat form: "text+image->text". Only the INPUT side (before `->`)
    // counts — an image-generating model isn't an image-reading one.
    if let Some(m) = arch["modality"].as_str() {
        let input = m.split("->").next().unwrap_or("");
        return if input.contains("image") {
            Vision::Supported
        } else {
            Vision::Unsupported
        };
    }

    Vision::Unknown
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// A capability store backed by a unique temp file. Because `STORE` is a
    /// process-wide `OnceLock` that can be set only once, tests can't use the
    /// global path; they drive the pure log helpers directly against a temp log.
    fn temp_log() -> JsonlLog {
        static N: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "lychi_modelcaps_test_{}_{}.jsonl",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_file(&path);
        JsonlLog::new(path)
    }

    fn cap(k: &str, vision: Vision, from_metadata: bool) -> ModelCapability {
        ModelCapability {
            key: k.to_string(),
            vision,
            from_metadata,
            estimate: None,
            updated_at: 1,
        }
    }

    #[test]
    fn key_is_provider_namespaced_and_normalised() {
        assert_eq!(key("Groq", " m "), "groq/m");
        assert_eq!(
            key("openrouter", "anthropic/claude"),
            "openrouter/anthropic/claude"
        );
    }

    #[test]
    fn folded_read_takes_the_last_line_per_key() {
        let log = temp_log();
        log.append(&cap("groq/m", Vision::Unknown, false)).unwrap();
        log.append(&cap("groq/m", Vision::Unsupported, false))
            .unwrap();
        let latest = latest_by_key(&log);
        assert_eq!(latest.get("groq/m").unwrap().vision, Vision::Unsupported);
        log.clear().unwrap();
    }

    #[test]
    fn append_and_compact_folds_duplicate_lines() {
        let log = temp_log();
        // Many upserts to the same key.
        for _ in 0..12 {
            append_and_maybe_compact(&log, cap("groq/m", Vision::Unsupported, false)).unwrap();
        }
        // After compaction the raw line count collapses toward one per key.
        assert!(
            log.approx_len().unwrap() <= 4,
            "expected compaction, got {} lines",
            log.approx_len().unwrap()
        );
        assert_eq!(
            latest_by_key(&log).get("groq/m").unwrap().vision,
            Vision::Unsupported
        );
        log.clear().unwrap();
    }

    #[test]
    fn a_vision_record_preserves_an_existing_estimate() {
        let log = temp_log();
        // Seed with an estimate.
        let mut with_est = cap("groq/m", Vision::Unknown, false);
        with_est.estimate = Some(Estimate {
            tier: Tier::Capable,
            params_label: "8B".into(),
            quant_label: "Q4".into(),
        });
        log.append(&with_est).unwrap();

        // Simulate `record` logic: carry the estimate forward on a vision update.
        let existing = current(&log, "groq/m");
        let updated = ModelCapability {
            vision: Vision::Unsupported,
            from_metadata: false,
            estimate: existing.and_then(|c| c.estimate),
            updated_at: 2,
            key: "groq/m".into(),
        };
        log.append(&updated).unwrap();

        let final_rec = current(&log, "groq/m").unwrap();
        assert_eq!(final_rec.vision, Vision::Unsupported);
        assert_eq!(final_rec.estimate.unwrap().tier, Tier::Capable);
        log.clear().unwrap();
    }

    #[test]
    fn unseen_model_folds_to_absent() {
        let log = temp_log();
        assert!(current(&log, "groq/never-seen").is_none());
        log.clear().unwrap();
    }

    #[test]
    fn openrouter_input_modalities_are_read() {
        let entry = json!({
            "id": "anthropic/claude-opus-5",
            "architecture": { "input_modalities": ["text", "image", "file"] }
        });
        assert_eq!(vision_from_model_json(&entry), Vision::Supported);

        let text_only = json!({
            "id": "some/text-model",
            "architecture": { "input_modalities": ["text"] }
        });
        assert_eq!(vision_from_model_json(&text_only), Vision::Unsupported);
    }

    #[test]
    fn a_bare_model_entry_yields_unknown_not_a_guess() {
        let entry = json!({ "id": "llama-3.3-70b-versatile", "object": "model" });
        assert_eq!(vision_from_model_json(&entry), Vision::Unknown);
    }

    #[test]
    fn only_the_input_side_of_a_flat_modality_counts() {
        let generator = json!({ "architecture": { "modality": "text->image" } });
        assert_eq!(vision_from_model_json(&generator), Vision::Unsupported);

        let reader = json!({ "architecture": { "modality": "text+image->text" } });
        assert_eq!(vision_from_model_json(&reader), Vision::Supported);
    }
}

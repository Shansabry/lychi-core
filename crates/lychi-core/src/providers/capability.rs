//! What the selected model can actually do — learned, never hardcoded.
//!
//! Lychi's model field is deliberately free-form (any id the endpoint accepts),
//! so there is no fixed list to consult and no provider table to maintain. This
//! module answers one question — "can this model see images?" — from two
//! sources, in order of confidence:
//!
//!   1. **Provider metadata.** Some endpoints report modalities on `/models`
//!      (OpenRouter's `architecture.input_modalities`). When present, that's
//!      authoritative and costs nothing.
//!   2. **Observed behaviour.** Most endpoints (OpenAI, Groq) list only model
//!      ids. There we start optimistic, and if a request is rejected *because*
//!      it carried images, we record that verdict so the next attach is warned
//!      before a request is wasted.
//!
//! The deliberate consequence: an unknown model fails ONCE, then never again.
//! That's the honest cost of not hardcoding a list — and unlike a list, it stays
//! correct as providers add vision to existing model ids.
//!
//! Unknown is a real answer here. `Vision::Unknown` means "no evidence either
//! way", and callers must treat it as *permission to try*, not as a refusal —
//! blocking on absent evidence would make every new model unusable.

use std::sync::Arc;

use redb::{Database, ReadableDatabase};
use serde::{Deserialize, Serialize};

use crate::db::MODEL_CAPS;
use crate::error::LychiError;

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

/// A learned capability record for one model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelCapability {
    pub vision: Vision,
    /// True when this came from provider metadata rather than an observed
    /// failure. Metadata wins if the two ever disagree.
    pub from_metadata: bool,
    pub updated_at: u64,
}

/// The process-wide capability store.
///
/// The provider factory is a pure config→provider function called from several
/// contexts (startup, a settings change, a reactor thread), none of which carry
/// a database handle. Threading one through every signature would spread a
/// storage concern across the whole provider stack for one optional feature, so
/// the app registers the store once at startup and the providers read it here.
/// Unset (in tests, or before init) simply means nothing is learned.
static STORE: std::sync::OnceLock<Arc<Database>> = std::sync::OnceLock::new();

/// Register the capability store. Called once, at app startup. Later calls are
/// ignored — the DB handle never changes during a run.
pub fn init_store(db: Arc<Database>) {
    let _ = STORE.set(db);
}

/// The registered store, if any.
pub fn store() -> Option<&'static Arc<Database>> {
    STORE.get()
}

/// The storage key. Namespaced by provider because the same model id can mean
/// different things across endpoints (a proxy may alias `gpt-4o` to anything).
fn key(provider: &str, model: &str) -> String {
    format!("{}/{}", provider.trim().to_lowercase(), model.trim())
}

/// Look up what we know about a model. `Unknown` when we've never seen it —
/// which callers must treat as "go ahead and try".
pub fn get_vision(db: &Arc<Database>, provider: &str, model: &str) -> Vision {
    if model.trim().is_empty() {
        return Vision::Unknown;
    }
    let k = key(provider, model);
    let Ok(txn) = db.begin_read() else {
        return Vision::Unknown;
    };
    let Ok(table) = txn.open_table(MODEL_CAPS) else {
        return Vision::Unknown;
    };
    match table.get(k.as_str()) {
        Ok(Some(v)) => postcard::from_bytes::<ModelCapability>(v.value())
            .map(|c| c.vision)
            .unwrap_or(Vision::Unknown),
        _ => Vision::Unknown,
    }
}

/// Record what we learned. Metadata is authoritative: an observed failure never
/// overwrites a metadata verdict, because a 400 can have causes other than
/// vision (a malformed request, a transient provider bug) while
/// `input_modalities` is a direct statement of capability.
pub fn record(
    db: &Arc<Database>,
    provider: &str,
    model: &str,
    vision: Vision,
    from_metadata: bool,
) -> Result<(), LychiError> {
    if model.trim().is_empty() || vision == Vision::Unknown {
        return Ok(()); // nothing worth storing
    }
    let k = key(provider, model);

    if !from_metadata && matches!(existing(db, &k), Some(c) if c.from_metadata) {
        return Ok(()); // don't let an observation override metadata
    }

    let entry = ModelCapability {
        vision,
        from_metadata,
        updated_at: crate::db::now_millis(),
    };
    let bytes = postcard::to_allocvec(&entry)
        .map_err(|e| LychiError::ExecutionFailed(format!("encode model capability: {e}")))?;

    let txn = db.begin_write()?;
    {
        let mut table = txn.open_table(MODEL_CAPS)?;
        table.insert(k.as_str(), bytes.as_slice())?;
    }
    txn.commit()?;
    Ok(())
}

fn existing(db: &Arc<Database>, k: &str) -> Option<ModelCapability> {
    let txn = db.begin_read().ok()?;
    let table = txn.open_table(MODEL_CAPS).ok()?;
    let v = table.get(k).ok()??;
    postcard::from_bytes::<ModelCapability>(v.value()).ok()
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

    fn db() -> Arc<Database> {
        crate::db::open_test_database()
    }

    #[test]
    fn an_unseen_model_is_unknown_not_refused() {
        // The critical default: no evidence must mean "try it", or a newly
        // released vision model would be unusable until Lychi shipped an update.
        assert_eq!(
            get_vision(&db(), "groq", "brand-new-model"),
            Vision::Unknown
        );
    }

    #[test]
    fn an_observed_failure_is_remembered() {
        let db = db();
        record(&db, "groq", "llama-3.3-70b", Vision::Unsupported, false).unwrap();
        assert_eq!(
            get_vision(&db, "groq", "llama-3.3-70b"),
            Vision::Unsupported
        );
    }

    #[test]
    fn metadata_beats_a_later_observation() {
        let db = db();
        record(&db, "openrouter", "m", Vision::Supported, true).unwrap();
        // A one-off 400 (malformed request, provider hiccup) must NOT flip a
        // capability the provider explicitly declared.
        record(&db, "openrouter", "m", Vision::Unsupported, false).unwrap();
        assert_eq!(get_vision(&db, "openrouter", "m"), Vision::Supported);
    }

    #[test]
    fn metadata_may_correct_an_earlier_observation() {
        let db = db();
        record(&db, "openrouter", "m", Vision::Unsupported, false).unwrap();
        record(&db, "openrouter", "m", Vision::Supported, true).unwrap();
        assert_eq!(get_vision(&db, "openrouter", "m"), Vision::Supported);
    }

    #[test]
    fn the_same_model_id_is_tracked_per_provider() {
        let db = db();
        record(&db, "groq", "shared-id", Vision::Unsupported, false).unwrap();
        // A proxy may alias the same id to a different model entirely.
        assert_eq!(get_vision(&db, "openrouter", "shared-id"), Vision::Unknown);
    }

    #[test]
    fn provider_lookup_is_case_insensitive() {
        let db = db();
        record(&db, "Groq", "m", Vision::Unsupported, false).unwrap();
        assert_eq!(get_vision(&db, "groq", "m"), Vision::Unsupported);
    }

    #[test]
    fn recording_unknown_or_empty_is_a_noop() {
        let db = db();
        record(&db, "groq", "m", Vision::Unknown, false).unwrap();
        assert_eq!(get_vision(&db, "groq", "m"), Vision::Unknown);
        record(&db, "groq", "", Vision::Unsupported, false).unwrap();
        assert_eq!(get_vision(&db, "groq", ""), Vision::Unknown);
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
        // Groq/OpenAI shape — no modality data at all.
        let entry = json!({ "id": "llama-3.3-70b-versatile", "object": "model" });
        assert_eq!(vision_from_model_json(&entry), Vision::Unknown);
    }

    #[test]
    fn only_the_input_side_of_a_flat_modality_counts() {
        // An image GENERATOR takes text in — it can't read an attachment.
        let generator = json!({ "architecture": { "modality": "text->image" } });
        assert_eq!(vision_from_model_json(&generator), Vision::Unsupported);

        let reader = json!({ "architecture": { "modality": "text+image->text" } });
        assert_eq!(vision_from_model_json(&reader), Vision::Supported);
    }
}

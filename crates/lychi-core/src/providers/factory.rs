//! Single source of truth for turning an [`AiConfig`] into a live
//! [`AiProvider`]. Every call site (app init, health check, hot-swap on save)
//! goes through [`build_provider`] so the mode-dispatch logic exists in exactly
//! one place instead of being copy-pasted across the Tauri layer.

use std::sync::Arc;

use crate::config::AiConfig;
use crate::providers::AiProvider;
use crate::providers::byo::{BYOClient, WireFormat};
use crate::providers::ollama::OllamaClient;

/// Why a provider could not be built. Callers surface these to the user (health
/// UI, logs) rather than silently getting `None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderError {
    /// AI is intentionally off (`mode = "disabled"` or unknown mode).
    Disabled,
    /// BYO mode but no API key is stored for the selected provider.
    MissingApiKey(String),
    /// BYO mode but no model has been chosen yet. Guarded here so a mid-setup
    /// health check or test never sends a request with `model: ""` — Groq
    /// answered that with a baffling 404 ("The model `` does not exist")
    /// during first-run setup, before the user reached the model field.
    ByoNoModel,
    /// BYO mode but the resolved base URL is empty (custom preset, no URL given).
    MissingBaseUrl,
    /// BYO mode but the base URL is unsafe (cleartext http to a non-loopback
    /// host, or a non-URL). Refused so the API key is never attached to it.
    UnsafeBaseUrl(String),
    /// Ollama mode but no model was selected.
    OllamaNoModel,
    /// Local mode but no model was selected.
    LocalNoModel,
    /// Local mode but the selected model file isn't downloaded yet.
    LocalModelMissing(String),
    /// Local mode requested but this build lacks the `local-ai` feature.
    LocalUnavailable,
    /// Cloud mode selected but no signed-in cloud client is available yet
    /// (user not signed in, or Lychi Cloud not enabled in this build).
    CloudUnavailable,
}

impl std::fmt::Display for ProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Disabled => write!(f, "AI is disabled"),
            Self::MissingApiKey(p) => write!(f, "No API key stored for '{p}'"),
            Self::ByoNoModel => write!(f, "No model selected yet — pick one to finish setup"),
            Self::MissingBaseUrl => {
                write!(f, "No endpoint URL set for the custom provider")
            }
            Self::UnsafeBaseUrl(m) => write!(f, "{m}"),
            Self::OllamaNoModel => write!(f, "No Ollama model selected"),
            Self::LocalNoModel => write!(f, "No local model selected"),
            Self::LocalModelMissing(m) => {
                write!(f, "Local model '{m}' is not downloaded yet")
            }
            Self::LocalUnavailable => {
                write!(f, "Local AI is not available in this build")
            }
            Self::CloudUnavailable => write!(f, "Lychi Cloud is not available yet"),
        }
    }
}

/// Build a live [`AiProvider`] from config, without cloud support.
///
/// Convenience wrapper over [`build_provider_with_cloud`] for call sites that
/// don't wire cloud (e.g. tests). Cloud mode yields [`ProviderError::CloudUnavailable`].
pub fn build_provider(
    cfg: &AiConfig,
    key_lookup: impl Fn(&str) -> Option<String>,
) -> Result<Arc<dyn AiProvider>, ProviderError> {
    build_provider_with_cloud(cfg, key_lookup, || None)
}

/// Build a live [`AiProvider`] from config — the single source of truth for
/// AI-mode dispatch.
///
/// Both provider-specific dependencies are injected so all of `lychi-core` stays
/// free of any keyring/OS-secret or Firebase dependency:
/// - `key_lookup(provider_id)` returns the stored BYO API key, or `None`.
/// - `cloud_provider()` builds a signed-in cloud client, or `None` if the user
///   isn't signed in / cloud isn't enabled. The Tauri layer supplies both.
pub fn build_provider_with_cloud(
    cfg: &AiConfig,
    key_lookup: impl Fn(&str) -> Option<String>,
    cloud_provider: impl Fn() -> Option<Arc<dyn AiProvider>>,
) -> Result<Arc<dyn AiProvider>, ProviderError> {
    match cfg.mode.as_str() {
        "byo" => build_byo(cfg, key_lookup),
        "ollama" => build_ollama(cfg),
        "local" => build_local(cfg),
        "cloud" => cloud_provider().ok_or(ProviderError::CloudUnavailable),
        // "disabled" and any unknown/legacy mode → off.
        _ => Err(ProviderError::Disabled),
    }
}

/// Build the bundled local-AI provider. Feature-gated: when `local-ai` is off,
/// a `mode = "local"` config degrades to `LocalUnavailable` rather than failing
/// the build (the arm still exists so old/foreign configs don't panic).
#[cfg(feature = "local-ai")]
fn build_local(cfg: &AiConfig) -> Result<Arc<dyn AiProvider>, ProviderError> {
    if cfg.local_model.trim().is_empty() {
        return Err(ProviderError::LocalNoModel);
    }
    let path = crate::paths::models_dir().join(&cfg.local_model);
    if !path.exists() {
        return Err(ProviderError::LocalModelMissing(cfg.local_model.clone()));
    }
    let client = crate::providers::local::LocalClient::load(path, cfg.max_tokens)
        .map_err(|e| ProviderError::LocalModelMissing(e.to_string()))?;
    Ok(Arc::new(client))
}

#[cfg(not(feature = "local-ai"))]
fn build_local(_cfg: &AiConfig) -> Result<Arc<dyn AiProvider>, ProviderError> {
    Err(ProviderError::LocalUnavailable)
}

fn build_byo(
    cfg: &AiConfig,
    key_lookup: impl Fn(&str) -> Option<String>,
) -> Result<Arc<dyn AiProvider>, ProviderError> {
    if cfg.model.trim().is_empty() {
        return Err(ProviderError::ByoNoModel);
    }
    let base_url = cfg.resolved_base_url();
    if base_url.is_empty() {
        return Err(ProviderError::MissingBaseUrl);
    }
    // Enforce the https-or-loopback policy HERE, at the point the API key is
    // about to be attached to requests for this URL — not only in the save
    // command. This holds even if config.toml is edited directly on disk: an
    // unsafe base_url yields a build error instead of silently exfiltrating the
    // key on the first request. Same decider as the save-time check (M1).
    cfg.validate_base_url()
        .map_err(ProviderError::UnsafeBaseUrl)?;
    let api_key = key_lookup(&cfg.provider)
        .filter(|k| !k.is_empty())
        .ok_or_else(|| ProviderError::MissingApiKey(cfg.provider.clone()))?;

    let wire = WireFormat::parse(&cfg.resolved_wire_format());
    let mut client = BYOClient::new(
        cfg.provider.clone(),
        wire,
        base_url,
        cfg.model.clone(),
        api_key,
        cfg.max_tokens,
    );
    // Learn from failures when a capability store is registered (it is, in the
    // app; not in tests). Wired here so no caller has to remember to.
    client = client.with_capability_learning(super::capability::store().is_some());
    Ok(Arc::new(client))
}

fn build_ollama(cfg: &AiConfig) -> Result<Arc<dyn AiProvider>, ProviderError> {
    if cfg.ollama_model.trim().is_empty() {
        return Err(ProviderError::OllamaNoModel);
    }
    let client = OllamaClient::new(
        cfg.ollama_url.clone(),
        cfg.ollama_model.clone(),
        cfg.max_tokens,
    );
    Ok(Arc::new(client))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byo_with_no_model_fails_the_build_with_a_plain_message() {
        // Mid-setup (provider picked, model field still empty) a health check
        // must get a setup hint, not send `model: ""` to the provider for a
        // baffling 404.
        let cfg = crate::config::AiConfig {
            mode: "byo".into(),
            provider: "groq".into(),
            model: String::new(),
            ..crate::config::AiConfig::default()
        };
        let err = match build_provider(&cfg, |_| Some("key".into())) {
            Err(e) => e,
            Ok(_) => panic!("empty model must not build"),
        };
        assert!(matches!(err, ProviderError::ByoNoModel), "{err}");
        assert!(err.to_string().contains("No model selected"), "{err}");
    }

    fn cfg(mode: &str) -> AiConfig {
        AiConfig {
            mode: mode.into(),
            ..Default::default()
        }
    }

    #[test]
    fn disabled_mode_returns_disabled() {
        let r = build_provider(&cfg("disabled"), |_| None);
        assert_eq!(r.err(), Some(ProviderError::Disabled));
    }

    #[test]
    fn unknown_mode_is_disabled_not_panic() {
        let r = build_provider(&cfg("legacy-nonsense"), |_| None);
        assert_eq!(r.err(), Some(ProviderError::Disabled));
    }

    #[test]
    fn cloud_mode_reports_unavailable() {
        let r = build_provider(&cfg("cloud"), |_| Some("k".into()));
        assert_eq!(r.err(), Some(ProviderError::CloudUnavailable));
    }

    #[test]
    fn byo_missing_key_reports_provider() {
        let mut c = cfg("byo");
        c.provider = "openai".into();
        let r = build_provider(&c, |_| None);
        assert_eq!(r.err(), Some(ProviderError::MissingApiKey("openai".into())));
    }

    #[test]
    fn byo_custom_without_url_errors() {
        let mut c = cfg("byo");
        c.provider = "custom".into();
        c.base_url = String::new();
        let r = build_provider(&c, |_| Some("k".into()));
        assert_eq!(r.err(), Some(ProviderError::MissingBaseUrl));
    }

    #[test]
    fn byo_unsafe_base_url_refuses_before_attaching_key() {
        // A cleartext-http remote base_url (as if config was edited on disk) must
        // fail the build, so the API key is never attached to it.
        let mut c = cfg("byo");
        c.provider = "custom".into();
        c.base_url = "http://attacker.example/v1".into();
        let r = build_provider(&c, |_| Some("real-key".into()));
        assert!(matches!(r.err(), Some(ProviderError::UnsafeBaseUrl(_))));

        // https is fine (it'll fail later for other reasons, but not UnsafeBaseUrl).
        let mut ok = cfg("byo");
        ok.provider = "custom".into();
        ok.base_url = "https://api.example.com/v1".into();
        let r = build_provider(&ok, |_| Some("real-key".into()));
        assert!(!matches!(r.err(), Some(ProviderError::UnsafeBaseUrl(_))));

        // loopback http is allowed (local proxy).
        let mut local = cfg("byo");
        local.provider = "custom".into();
        local.base_url = "http://localhost:11434/v1".into();
        let r = build_provider(&local, |_| Some("real-key".into()));
        assert!(!matches!(r.err(), Some(ProviderError::UnsafeBaseUrl(_))));
    }

    #[test]
    fn byo_known_preset_builds_with_key() {
        let mut c = cfg("byo");
        c.provider = "grok".into();
        c.model = "grok-2".into();
        let r = build_provider(&c, |p| {
            assert_eq!(p, "grok");
            Some("sk-test".into())
        });
        let p = r.expect("should build");
        assert_eq!(p.name(), "grok");
    }

    #[test]
    fn byo_custom_with_url_builds() {
        let mut c = cfg("byo");
        c.provider = "custom".into();
        c.base_url = "https://my-proxy.local/v1/chat/completions".into();
        c.model = "local-model".into();
        let r = build_provider(&c, |_| Some("sk".into()));
        assert!(r.is_ok());
    }

    #[test]
    fn ollama_without_model_errors() {
        let c = cfg("ollama");
        let r = build_provider(&c, |_| None);
        assert_eq!(r.err(), Some(ProviderError::OllamaNoModel));
    }

    #[test]
    fn ollama_with_model_builds() {
        let mut c = cfg("ollama");
        c.ollama_model = "llama3:8b".into();
        let r = build_provider(&c, |_| None);
        assert!(r.is_ok());
        assert_eq!(r.unwrap().name(), "ollama");
    }
}

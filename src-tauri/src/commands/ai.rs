use crate::state::AppState;
use lychi_core::config::AiConfig;
use lychi_core::error::LychiError;
use lychi_core::paths;
use tauri::State;

#[tauri::command]
#[specta::specta]
pub async fn get_ai_config(state: State<'_, AppState>) -> Result<AiConfig, LychiError> {
    let config = state.config.read().await;
    Ok(config.ai.clone())
}

#[tauri::command]
#[specta::specta]
pub async fn save_ai_config(
    state: State<'_, AppState>,
    ai_config: AiConfig,
) -> Result<(), LychiError> {
    use lychi_core::config::db as config_db;
    use lychi_core::events::{ConfigSection, DomainEvent};

    // Persist, then release the write lock BEFORE emitting — the AiReactor
    // acquires the config/executor locks with blocking_*, so this command must
    // not still hold them when the event fans out.
    {
        let mut config = state.config.write().await;
        config.ai = ai_config;
        config.save(&paths::config_file())?;
        config_db::save_config_to_db(&state.db, &config)?;
    }

    // The AiReactor rebuilds the provider, hot-swaps the router, and
    // re-registers the AI-dependent handlers — no restart needed to switch.
    //
    // Emit on a blocking thread: the reactor reads the OS keyring (whose
    // secret-service backend spins its own runtime) and takes locks with
    // `blocking_*`. Running it on a tokio async worker would panic ("Cannot
    // start a runtime from within a runtime"). `EventBus::emit` is synchronous,
    // so we hop off the worker here.
    let bus = state.event_bus.clone();
    let _ = tauri::async_runtime::spawn_blocking(move || {
        bus.emit(DomainEvent::ConfigChanged {
            section: ConfigSection::Ai,
        });
    })
    .await;

    Ok(())
}

#[tauri::command]
#[specta::specta]
pub async fn set_api_key(provider: String, key: String) -> Result<(), LychiError> {
    // keyring calls are blocking D-Bus round-trips on Linux — run on a blocking thread
    // with a 5s timeout to guard against a hung secret-service daemon.
    let task = tauri::async_runtime::spawn_blocking(move || set_api_key_sync(&provider, &key));
    match tokio::time::timeout(std::time::Duration::from_secs(5), task).await {
        Ok(Ok(result)) => result,
        Ok(Err(e)) => Err(LychiError::Config(format!("keyring task panicked: {e}"))),
        Err(_) => Err(LychiError::Config(
            "keyring timed out after 5s — secret-service daemon may be unresponsive".into(),
        )),
    }
}

fn set_api_key_sync(provider: &str, key: &str) -> Result<(), LychiError> {
    let entry = keyring::Entry::new("lychi", &format!("byo-{provider}"))
        .map_err(|e| LychiError::Config(format!("Keyring error: {e}")))?;
    if key.is_empty() {
        // Delete the key from the keyring
        let _ = entry.delete_credential(); // ignore error if not found
        Ok(())
    } else {
        entry
            .set_password(key)
            .map_err(|e| LychiError::Config(format!("Failed to store API key: {e}")))
    }
}

#[tauri::command]
#[specta::specta]
pub async fn get_ai_status(state: State<'_, AppState>) -> Result<AiStatus, LychiError> {
    let config = state.config.read().await;
    let executor = state.executor.read().await;
    Ok(AiStatus {
        mode: config.ai.mode.clone(),
        provider: config.ai.provider.clone(),
        model: config.ai.model.clone(),
        has_ai_router: executor.has_ai(),
    })
}

#[tauri::command]
#[specta::specta]
pub async fn check_ai_health(state: State<'_, AppState>) -> Result<bool, LychiError> {
    let config = state.config.read().await;
    tracing::debug!(
        "check_ai_health: mode={}, provider={}",
        config.ai.mode,
        config.ai.provider
    );
    // Build a temporary provider via the same factory the app uses, then probe
    // it. Single source of truth for mode dispatch — no per-mode duplication.
    let ai = config.ai.clone();
    drop(config);

    match crate::state::AppState::build_ai_provider_async(ai).await {
        Some(provider) => Ok(provider.health_check().await),
        None => Ok(false),
    }
}

/// Result of a live connection test — a real round-trip, not just a reachability
/// ping. `ok` means the endpoint accepted the request AND the model produced a
/// reply; `error` carries the reason (bad key, unknown model, unreachable, …)
/// so the UI can show it inline.
#[derive(serde::Serialize, specta::Type)]
pub struct AiTestResult {
    pub ok: bool,
    pub error: Option<String>,
}

/// Actively test the configured AI provider by sending one real request through
/// it. Unlike `check_ai_health` (which pings `/models` and passes even when the
/// *model name* is wrong), this exercises the full path — endpoint, auth, AND
/// model — so a typo'd free-form model id is caught here.
#[tauri::command]
#[specta::specta]
pub async fn test_ai_connection(state: State<'_, AppState>) -> Result<AiTestResult, LychiError> {
    let ai = { state.config.read().await.ai.clone() };

    let Some(provider) = crate::state::AppState::build_ai_provider_async(ai.clone()).await else {
        return Ok(AiTestResult {
            ok: false,
            error: Some("AI is not configured — set a mode, model, and API key first.".into()),
        });
    };

    // A trivial prompt that forces a real inference call with the chosen model.
    let fut = provider.answer_question(
        "You are a connection test. Reply with the single word: ok.",
        "ping",
    );
    // Bound the test so a hung endpoint doesn't wedge the settings UI.
    let timeout = std::time::Duration::from_secs(ai.timeout_secs.clamp(2, 30));
    match tokio::time::timeout(timeout, fut).await {
        Ok(Ok(_reply)) => Ok(AiTestResult {
            ok: true,
            error: None,
        }),
        Ok(Err(e)) => Ok(AiTestResult {
            ok: false,
            error: Some(e.to_string()),
        }),
        Err(_) => Ok(AiTestResult {
            ok: false,
            error: Some(format!("Timed out after {}s", timeout.as_secs())),
        }),
    }
}

#[tauri::command]
#[specta::specta]
pub async fn list_ollama_models(
    state: State<'_, AppState>,
) -> Result<Vec<lychi_core::providers::ollama::OllamaModelInfo>, LychiError> {
    let config = state.config.read().await;
    lychi_core::providers::ollama::OllamaClient::list_models(&config.ai.ollama_url).await
}

#[tauri::command]
#[specta::specta]
pub async fn get_masked_api_key(provider: String) -> Result<Option<String>, LychiError> {
    // keyring calls are blocking D-Bus round-trips on Linux — run on a blocking thread
    // with a 5s timeout to guard against a hung secret-service daemon.
    let task = tauri::async_runtime::spawn_blocking(move || match get_stored_key(&provider) {
        Ok(key) => Ok(Some(mask_key(&key))),
        Err(_) => Ok(None),
    });
    match tokio::time::timeout(std::time::Duration::from_secs(5), task).await {
        Ok(Ok(result)) => result,
        Ok(Err(e)) => Err(LychiError::Config(format!("keyring task panicked: {e}"))),
        Err(_) => {
            tracing::warn!("get_masked_api_key timed out — returning None");
            Ok(None) // Treat timeout as "no key found" so UI doesn't break
        }
    }
}

/// Mask an API key for display: show first 4 and last 4 chars with "..." in the middle.
/// Short keys (<=10 chars) show first 3 + "..." + last 3.
fn mask_key(key: &str) -> String {
    let len = key.len();
    if len <= 6 {
        return "*".repeat(len);
    }
    if len <= 10 {
        format!("{}...{}", &key[..3], &key[len - 3..])
    } else {
        format!("{}...{}", &key[..4], &key[len - 4..])
    }
}

fn get_stored_key(provider: &str) -> Result<String, LychiError> {
    let entry = keyring::Entry::new("lychi", &format!("byo-{provider}"))
        .map_err(|e| LychiError::Config(format!("Keyring error: {e}")))?;
    entry
        .get_password()
        .map_err(|e| LychiError::Config(format!("No API key found for {provider}: {e}")))
}

#[derive(serde::Serialize, specta::Type)]
pub struct AiStatus {
    pub mode: String,
    pub provider: String,
    pub model: String,
    pub has_ai_router: bool,
}

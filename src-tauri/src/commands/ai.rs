use crate::state::AppState;
use lychi_core::config::AiConfig;
use lychi_core::error::LychiError;
use lychi_core::paths;
use tauri::State;

#[tauri::command]
pub async fn get_ai_config(state: State<'_, AppState>) -> Result<AiConfig, LychiError> {
    let config = state.config.read().await;
    Ok(config.ai.clone())
}

#[tauri::command]
pub async fn save_ai_config(
    state: State<'_, AppState>,
    ai_config: AiConfig,
) -> Result<(), LychiError> {
    let mut config = state.config.write().await;
    config.ai = ai_config;
    config.save(&paths::config_file())
}

#[tauri::command]
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
pub async fn check_ai_health(state: State<'_, AppState>) -> Result<bool, LychiError> {
    let config = state.config.read().await;
    tracing::debug!(
        "check_ai_health: mode={}, provider={}",
        config.ai.mode,
        config.ai.provider
    );
    if config.ai.mode == "disabled" {
        return Ok(false);
    }

    // Build a temporary provider to check health
    if config.ai.mode == "byo" {
        let key = get_stored_key(&config.ai.provider);
        match &key {
            Ok(_) => tracing::debug!("API key found for {}", config.ai.provider),
            Err(e) => tracing::warn!("No API key for {}: {e}", config.ai.provider),
        }
        if let Ok(key) = key {
            let provider: lychi_core::providers::byo::BYOProvider = config.ai.provider.parse()?;
            let client = lychi_core::providers::byo::BYOClient::new(
                provider,
                config.ai.model.clone(),
                key,
                config.ai.max_tokens,
            );
            use lychi_core::providers::AiProvider;
            return Ok(client.health_check().await);
        }
    }

    Ok(false)
}

#[tauri::command]
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

#[derive(serde::Serialize)]
pub struct AiStatus {
    pub mode: String,
    pub provider: String,
    pub model: String,
    pub has_ai_router: bool,
}

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use lychi_core::error::LychiError;
use lychi_core::providers::cloud::{CloudClient, CreditBalance, TokenProvider};
use tauri::{AppHandle, Emitter, State};

use crate::state::AppState;

// Firebase Web API key — safe to embed in desktop binary (public identifier,
// not a secret). Obtained from Firebase Console → Project Settings → Web app.
pub const FIREBASE_API_KEY: &str = "AIzaSyBPlaceholder_ReplaceWithRealKey";

const FIREBASE_REFRESH_URL: &str = "https://securetoken.googleapis.com/v1/token";
const CLOUD_BASE_URL: &str = "https://api.lychi.app";

// Keyring entry names (service: "lychi")
const KEY_ID_TOKEN: &str = "firebase-id_token";
const KEY_REFRESH_TOKEN: &str = "firebase-refresh_token";
const KEY_ISSUED_AT: &str = "firebase-issued_at";
const KEY_USER_EMAIL: &str = "firebase-user_email";
const KEY_USER_UID: &str = "firebase-user_uid";

// Refresh the token if it's older than 55 minutes (Firebase tokens expire at 1hr)
const REFRESH_THRESHOLD_SECS: u64 = 55 * 60;

// ── FirebaseUser ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct FirebaseUser {
    pub uid: String,
    pub email: String,
}

// ── Keyring helpers ─────────────────────────────────────────────────────────

fn keyring_get_sync(key: &str) -> Option<String> {
    let entry = keyring::Entry::new("lychi", key).ok()?;
    entry.get_password().ok()
}

fn keyring_set_sync(key: &str, value: &str) -> Result<(), String> {
    let entry = keyring::Entry::new("lychi", key).map_err(|e| format!("Keyring error: {e}"))?;
    entry
        .set_password(value)
        .map_err(|e| format!("Keyring set: {e}"))
}

fn keyring_delete_sync(key: &str) {
    if let Ok(entry) = keyring::Entry::new("lychi", key) {
        let _ = entry.delete_credential();
    }
}

/// Run a keyring operation with 5s timeout to guard against hung D-Bus daemon.
async fn with_keyring_timeout<F, T>(f: F) -> Result<T, LychiError>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let task = tauri::async_runtime::spawn_blocking(f);
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .map_err(|_| LychiError::Config("keyring timed out".into()))?
        .map_err(|e| LychiError::Config(format!("keyring task panicked: {e}")))
}

// ── KeyringTokenProvider ────────────────────────────────────────────────────

/// Implements `TokenProvider` using the OS keyring. Stores tokens in keyring
/// entries, auto-refreshes when tokens are close to expiry.
pub struct KeyringTokenProvider {
    http: reqwest::Client,
}

impl Default for KeyringTokenProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl KeyringTokenProvider {
    pub fn new() -> Self {
        Self {
            http: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(3))
                .timeout(Duration::from_secs(10))
                .build()
                .unwrap_or_default(),
        }
    }

    /// Returns true if the user has valid tokens stored.
    pub fn is_signed_in(&self) -> bool {
        keyring_get_sync(KEY_ID_TOKEN).is_some() && keyring_get_sync(KEY_REFRESH_TOKEN).is_some()
    }

    /// Read age of stored ID token in seconds. Returns u64::MAX if missing.
    fn token_age_secs() -> u64 {
        let issued_at: u64 = match keyring_get_sync(KEY_ISSUED_AT) {
            Some(s) => s.parse().unwrap_or(0),
            None => return u64::MAX,
        };
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        now.saturating_sub(issued_at)
    }

    /// Call Firebase token refresh endpoint with a refresh token.
    async fn refresh_tokens(&self, refresh_token: &str) -> Result<(String, String), LychiError> {
        let url = format!("{FIREBASE_REFRESH_URL}?key={FIREBASE_API_KEY}");

        let resp = self
            .http
            .post(&url)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(format!(
                "grant_type=refresh_token&refresh_token={refresh_token}"
            ))
            .send()
            .await
            .map_err(|e| LychiError::Ai(format!("Token refresh failed: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(LychiError::Ai(format!("Token refresh {status}: {body}")));
        }

        let json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| LychiError::Ai(format!("Parse refresh response: {e}")))?;

        let new_id_token = json["id_token"]
            .as_str()
            .ok_or_else(|| LychiError::Ai("No id_token in refresh response".to_string()))?
            .to_string();
        let new_refresh_token = json["refresh_token"]
            .as_str()
            .ok_or_else(|| LychiError::Ai("No refresh_token in response".to_string()))?
            .to_string();

        Ok((new_id_token, new_refresh_token))
    }
}

#[async_trait]
impl TokenProvider for KeyringTokenProvider {
    async fn get_token(&self) -> Result<String, LychiError> {
        // Fast path: token is fresh, return cached
        let age = Self::token_age_secs();
        if age < REFRESH_THRESHOLD_SECS
            && let Some(token) = keyring_get_sync(KEY_ID_TOKEN)
        {
            return Ok(token);
        }

        // Slow path: refresh
        let refresh_token = keyring_get_sync(KEY_REFRESH_TOKEN)
            .ok_or_else(|| LychiError::Ai("Not signed in to Cloud".to_string()))?;

        let (new_id, new_refresh) = self.refresh_tokens(&refresh_token).await?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        // Store refreshed tokens (blocking keyring calls are OK here, we're in an async context)
        let new_id_clone = new_id.clone();
        tauri::async_runtime::spawn_blocking(move || {
            let _ = keyring_set_sync(KEY_ID_TOKEN, &new_id_clone);
            let _ = keyring_set_sync(KEY_REFRESH_TOKEN, &new_refresh);
            let _ = keyring_set_sync(KEY_ISSUED_AT, &now.to_string());
        })
        .await
        .map_err(|e| LychiError::Ai(format!("Failed to save tokens: {e}")))?;

        Ok(new_id)
    }
}

// ── IPC commands ────────────────────────────────────────────────────────────

/// Handle a deep link callback: `lychi://auth-callback?id_token=...&refresh_token=...&email=...&uid=...`
/// Called from the deep-link handler in lib.rs.
pub async fn handle_auth_callback(app: &AppHandle, url: &str) -> Result<(), LychiError> {
    // Parse query params
    let parsed = url::Url::parse(url)
        .map_err(|e| LychiError::Config(format!("Invalid callback URL: {e}")))?;

    let mut id_token: Option<String> = None;
    let mut refresh_token: Option<String> = None;
    let mut email: Option<String> = None;
    let mut uid: Option<String> = None;

    for (key, value) in parsed.query_pairs() {
        match key.as_ref() {
            "id_token" => id_token = Some(value.into_owned()),
            "refresh_token" => refresh_token = Some(value.into_owned()),
            "email" => email = Some(value.into_owned()),
            "uid" => uid = Some(value.into_owned()),
            _ => {}
        }
    }

    let id_token = id_token.ok_or_else(|| LychiError::Config("Missing id_token".into()))?;
    let refresh_token =
        refresh_token.ok_or_else(|| LychiError::Config("Missing refresh_token".into()))?;
    let email = email.unwrap_or_default();
    let uid = uid.unwrap_or_default();

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    // Store in keyring
    let email_for_keyring = email.clone();
    with_keyring_timeout(move || -> Result<(), String> {
        keyring_set_sync(KEY_ID_TOKEN, &id_token)?;
        keyring_set_sync(KEY_REFRESH_TOKEN, &refresh_token)?;
        keyring_set_sync(KEY_ISSUED_AT, &now.to_string())?;
        keyring_set_sync(KEY_USER_EMAIL, &email_for_keyring)?;
        keyring_set_sync(KEY_USER_UID, &uid)?;
        Ok(())
    })
    .await?
    .map_err(LychiError::Config)?;

    // Emit event to frontend so it can refresh UI
    let _ = app.emit("lychi://firebase-signed-in", ());

    tracing::info!("Firebase auth: signed in as {email}");
    Ok(())
}

/// Open the hosted sign-in page in the system browser.
#[tauri::command]
#[specta::specta]
pub async fn firebase_sign_in() -> Result<(), LychiError> {
    let url = "https://api.lychi.app/auth/signin".to_string();
    crate::commands::open_uri::open_uri(url).await
}

/// Return the currently signed-in user, or None if not signed in.
#[tauri::command]
#[specta::specta]
pub async fn firebase_get_user() -> Result<Option<FirebaseUser>, LychiError> {
    with_keyring_timeout(|| {
        let id_token = keyring_get_sync(KEY_ID_TOKEN);
        let email = keyring_get_sync(KEY_USER_EMAIL).unwrap_or_default();
        let uid = keyring_get_sync(KEY_USER_UID).unwrap_or_default();

        if id_token.is_some() && !uid.is_empty() {
            Some(FirebaseUser { uid, email })
        } else {
            None
        }
    })
    .await
}

/// Sign out: clear all Firebase tokens from keyring.
#[tauri::command]
#[specta::specta]
pub async fn firebase_sign_out(app: AppHandle) -> Result<(), LychiError> {
    with_keyring_timeout(|| {
        keyring_delete_sync(KEY_ID_TOKEN);
        keyring_delete_sync(KEY_REFRESH_TOKEN);
        keyring_delete_sync(KEY_ISSUED_AT);
        keyring_delete_sync(KEY_USER_EMAIL);
        keyring_delete_sync(KEY_USER_UID);
    })
    .await?;

    let _ = app.emit("lychi://firebase-signed-out", ());
    tracing::info!("Firebase auth: signed out");
    Ok(())
}

/// Fetch the user's cloud credit balance.
#[tauri::command]
#[specta::specta]
pub async fn cloud_get_credits(_state: State<'_, AppState>) -> Result<CreditBalance, LychiError> {
    let provider = Arc::new(KeyringTokenProvider::new());
    let client = CloudClient::new(CLOUD_BASE_URL.to_string(), provider);
    client.get_credits().await
}

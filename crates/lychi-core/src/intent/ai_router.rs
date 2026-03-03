use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::error::LychiError;
use crate::providers::{AiProvider, AiResponse, AiRoute};

/// A single cached AI routing result with an expiry timestamp.
struct CacheEntry {
    response: AiResponse,
    expires_at: Instant,
}

/// State for the health gate — tracks recent failures to avoid hammering a broken provider.
struct HealthGate {
    /// Time when the backoff window expires (None = no backoff active).
    backoff_until: Option<Instant>,
    /// Number of consecutive failures (resets on success).
    consecutive_failures: u32,
}

impl HealthGate {
    fn new() -> Self {
        Self {
            backoff_until: None,
            consecutive_failures: 0,
        }
    }

    /// Returns true if we should skip the AI call due to recent failures.
    fn is_backing_off(&self) -> bool {
        self.backoff_until.is_some_and(|t| Instant::now() < t)
    }

    fn record_success(&mut self) {
        self.consecutive_failures = 0;
        self.backoff_until = None;
    }

    fn record_failure(&mut self) {
        self.consecutive_failures += 1;
        // Exponential backoff: 2s, 4s, 8s, capped at 60s
        let secs = (2u64).saturating_pow(self.consecutive_failures).min(60);
        self.backoff_until = Some(Instant::now() + Duration::from_secs(secs));
        tracing::warn!(
            "AI routing failed {} time(s) in a row — backing off for {secs}s",
            self.consecutive_failures
        );
    }
}

/// Wraps an active AI provider with timeout, result caching, and health gate.
pub struct AiRouter {
    provider: Arc<dyn AiProvider>,
    timeout: Duration,
    /// Optional context hint for AI routing (set by Executor before resolve).
    context_hint: Mutex<Option<String>>,
    /// Short-lived cache for context-free routing results.
    /// Key: normalized input string. Only populated when no context hint is present.
    cache: Mutex<HashMap<String, CacheEntry>>,
    /// Health gate — tracks consecutive failures to avoid hammering broken providers.
    health: Mutex<HealthGate>,
}

/// How long a cached route stays valid.
const CACHE_TTL: Duration = Duration::from_secs(300); // 5 minutes

impl AiRouter {
    pub fn new(provider: Box<dyn AiProvider>) -> Self {
        Self {
            provider: Arc::from(provider),
            timeout: Duration::from_secs(8),
            context_hint: Mutex::new(None),
            cache: Mutex::new(HashMap::new()),
            health: Mutex::new(HealthGate::new()),
        }
    }

    pub fn new_shared(provider: Arc<dyn AiProvider>) -> Self {
        Self {
            provider,
            timeout: Duration::from_secs(8),
            context_hint: Mutex::new(None),
            cache: Mutex::new(HashMap::new()),
            health: Mutex::new(HealthGate::new()),
        }
    }

    /// Set the context hint for the next AI routing call.
    pub fn set_context_hint(&self, hint: Option<String>) {
        if let Ok(mut guard) = self.context_hint.lock() {
            *guard = hint;
        }
    }

    /// Take the current context hint (consuming it).
    fn take_context_hint(&self) -> Option<String> {
        self.context_hint.lock().ok().and_then(|mut g| g.take())
    }

    /// Normalize input for use as a cache key.
    fn cache_key(input: &str) -> String {
        input.trim().to_lowercase()
    }

    /// Check cache for a previously resolved route. Returns `None` if not cached or expired.
    fn cache_get(&self, key: &str) -> Option<AiResponse> {
        let mut cache = self.cache.lock().ok()?;
        let entry = cache.get(key)?;
        if Instant::now() < entry.expires_at {
            Some(entry.response.clone())
        } else {
            cache.remove(key);
            None
        }
    }

    /// Store a successful route in the cache.
    fn cache_set(&self, key: String, response: AiResponse) {
        if let Ok(mut cache) = self.cache.lock() {
            // Evict expired entries periodically (simple: if cache > 200 entries, clear all)
            if cache.len() > 200 {
                cache.retain(|_, v| Instant::now() < v.expires_at);
            }
            cache.insert(
                key,
                CacheEntry {
                    response,
                    expires_at: Instant::now() + CACHE_TTL,
                },
            );
        }
    }

    /// Route natural language input through the AI provider (single-shot only).
    /// Returns `Ok(Some(route))` on success, `Ok(None)` on timeout/failure/backoff.
    pub async fn try_route(
        &self,
        input: &str,
        known_actions: &[&str],
    ) -> Result<Option<AiRoute>, LychiError> {
        match self.try_route_or_plan(input, known_actions).await? {
            Some(AiResponse::SingleRoute(route)) => Ok(Some(route)),
            Some(AiResponse::Plan(_)) => {
                tracing::debug!("AI returned plan but single route expected, falling back");
                Ok(None)
            }
            None => Ok(None),
        }
    }

    /// Route input, returning either a single route or a multi-step plan.
    /// Returns `Ok(None)` on timeout/failure/backoff.
    pub async fn try_route_or_plan(
        &self,
        input: &str,
        known_actions: &[&str],
    ) -> Result<Option<AiResponse>, LychiError> {
        let hint = self.take_context_hint();

        // Health gate: skip AI call if provider is in backoff
        if self.health.lock().is_ok_and(|h| h.is_backing_off()) {
            tracing::debug!("AI routing skipped — provider in backoff");
            return Ok(None);
        }

        // Cache lookup: only for context-free queries (context changes the answer)
        let cache_key = if hint.is_none() {
            let key = Self::cache_key(input);
            if let Some(cached) = self.cache_get(&key) {
                tracing::debug!("AI cache hit for '{input}'");
                return Ok(Some(cached));
            }
            Some(key)
        } else {
            None
        };

        let result = tokio::time::timeout(
            self.timeout,
            self.provider
                .route_or_plan(input, known_actions, hint.as_deref()),
        )
        .await;

        match result {
            Ok(Ok(response)) => {
                match &response {
                    AiResponse::SingleRoute(route) => {
                        tracing::info!(
                            action = %route.action_id,
                            prompt_version = crate::intent::prompt::PROMPT_VERSION,
                            "[ai] routed '{}' → {} {:?}",
                            input,
                            route.action_id,
                            route.args
                        );
                    }
                    AiResponse::Plan(plan) => {
                        tracing::info!(
                            steps = plan.steps.len(),
                            prompt_version = crate::intent::prompt::PROMPT_VERSION,
                            "[ai] planned '{}' → {} steps",
                            input,
                            plan.steps.len()
                        );
                    }
                }
                // Record success and cache result (context-free only)
                if let Ok(mut health) = self.health.lock() {
                    health.record_success();
                }
                if let Some(key) = cache_key {
                    self.cache_set(key, response.clone());
                }
                Ok(Some(response))
            }
            Ok(Err(e)) => {
                tracing::warn!("AI routing failed: {e}");
                if let Ok(mut health) = self.health.lock() {
                    health.record_failure();
                }
                Ok(None)
            }
            Err(_) => {
                tracing::warn!("AI routing timed out after {:?}", self.timeout);
                if let Ok(mut health) = self.health.lock() {
                    health.record_failure();
                }
                Ok(None)
            }
        }
    }

    /// Check if the underlying provider is healthy.
    pub async fn health_check(&self) -> bool {
        self.provider.health_check().await
    }

    /// Whether the router is currently in backoff (recent failures).
    pub fn is_backing_off(&self) -> bool {
        self.health.lock().is_ok_and(|h| h.is_backing_off())
    }

    /// Provider name for display.
    pub fn provider_name(&self) -> &str {
        self.provider.name()
    }
}

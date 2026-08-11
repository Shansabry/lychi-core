use std::sync::Arc;
use std::time::Duration;

use crate::providers::AiProvider;

/// Holds the active AI provider. Its PRESENCE is the "AI is configured" signal
/// the resolver reads via `has_ai()` to gate natural-language routing (NL →
/// agent vs. NL → web). The provider itself is driven only through the
/// streaming `chat()` primitive, by the coordinator agent — there is no
/// routing/planning layer here any more (removed with the legacy
/// `route_or_plan`/`answer_question` surface; the agent is the one AI path).
///
/// Kept as a named type rather than a bare `Arc<dyn AiProvider>` so the
/// reactor's construction site and the `Option<AiRouter>` availability flag
/// read clearly, and so a future health/backoff holder has an obvious home.
pub struct AiRouter {
    #[allow(dead_code)] // held for lifetime + future health surface; presence is the signal
    provider: Arc<dyn AiProvider>,
    #[allow(dead_code)]
    timeout: Duration,
}

impl AiRouter {
    pub fn new(provider: Box<dyn AiProvider>, timeout: Duration) -> Self {
        Self {
            provider: Arc::from(provider),
            timeout,
        }
    }

    pub fn new_shared(provider: Arc<dyn AiProvider>, timeout: Duration) -> Self {
        Self { provider, timeout }
    }
}

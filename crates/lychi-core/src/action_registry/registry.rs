use std::collections::HashMap;

use crate::action_registry::{ActionHandler, CompletionItem};

/// Pure action registry — stores and looks up handlers. No routing, no AI, no execution.
pub struct ActionRegistry {
    handlers: HashMap<String, Box<dyn ActionHandler>>,
}

impl ActionRegistry {
    pub fn new() -> Self {
        Self {
            handlers: HashMap::new(),
        }
    }

    pub fn register(&mut self, handler: Box<dyn ActionHandler>) {
        self.handlers.insert(handler.id().to_string(), handler);
    }

    pub fn get(&self, id: &str) -> Option<&dyn ActionHandler> {
        self.handlers.get(id).map(|h| h.as_ref())
    }

    pub fn list_ids(&self) -> Vec<&str> {
        self.handlers.keys().map(|s| s.as_str()).collect()
    }

    /// Return (id, description) pairs for all handlers. Used by the AI prompt builder.
    pub fn list_descriptions(&self) -> Vec<(&str, &str)> {
        self.handlers
            .values()
            .map(|h| (h.id(), h.description()))
            .collect()
    }

    pub fn has(&self, id: &str) -> bool {
        self.handlers.contains_key(id)
    }

    /// Get completions from a specific handler.
    pub async fn completions(&self, handler_id: &str, partial: &str) -> Vec<CompletionItem> {
        if let Some(handler) = self.handlers.get(handler_id) {
            handler.completions(partial).await
        } else {
            Vec::new()
        }
    }
}

impl Default for ActionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action_registry::{ActionHandler, ActionResult, RiskLevel};
    use crate::error::LychiError;
    use async_trait::async_trait;

    struct DummyHandler;

    #[async_trait]
    impl ActionHandler for DummyHandler {
        fn id(&self) -> &str {
            "test"
        }
        fn description(&self) -> &str {
            "A test action"
        }
        async fn execute(&self, args: &str) -> Result<ActionResult, LychiError> {
            Ok(ActionResult {
                success: true,
                output: Some(format!("executed with: {args}")),
                error: None,
                duration_ms: 0,
                routed_by: None,
                open_url: None,
                needs_confirmation: None,
                risk_level: None,
                output_type: None,
                executed_args: None,
                launch_desktop: None,
                focus_app: None,
            })
        }
    }

    #[test]
    fn register_and_lookup() {
        let mut registry = ActionRegistry::new();
        registry.register(Box::new(DummyHandler));
        assert!(registry.has("test"));
        assert!(!registry.has("nope"));
        assert_eq!(registry.get("test").unwrap().id(), "test");
        assert_eq!(registry.get("test").unwrap().default_risk(), RiskLevel::Low);
    }

    #[test]
    fn list_ids() {
        let mut registry = ActionRegistry::new();
        registry.register(Box::new(DummyHandler));
        let ids = registry.list_ids();
        assert!(ids.contains(&"test"));
    }
}

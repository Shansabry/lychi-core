use std::collections::HashMap;

use crate::action_registry::trigger::ArgTransform;
use crate::action_registry::{ActionHandler, CommandInfo, CompletionItem};

/// A resolved routing entry: which handler a keyword prefix maps to, and how to
/// shape the remaining text into that handler's args.
#[derive(Clone)]
struct PrefixRoute {
    handler_id: String,
    transform: ArgTransform,
}

/// Pure action registry — stores and looks up handlers. Also owns the keyword→
/// handler routing index, built from each handler's declared `triggers()` so the
/// routing table can't drift from the handler set.
pub struct ActionRegistry {
    handlers: HashMap<String, Box<dyn ActionHandler>>,
    /// keyword (lowercase) → route. Rebuilt whenever a handler is registered.
    prefix_index: HashMap<String, PrefixRoute>,
}

impl ActionRegistry {
    pub fn new() -> Self {
        Self {
            handlers: HashMap::new(),
            prefix_index: HashMap::new(),
        }
    }

    pub fn register(&mut self, handler: Box<dyn ActionHandler>) {
        // Index this handler's keyword triggers. Registering a handler with an
        // id that already exists (hot-reload of shell/bang/project handlers)
        // replaces both the handler and its prefix routes.
        let id = handler.id().to_string();
        // Drop any stale routes that pointed at this id (in case its triggers changed).
        self.prefix_index.retain(|_, r| r.handler_id != id);
        for trigger in handler.triggers() {
            for prefix in trigger.prefixes {
                self.prefix_index.insert(
                    prefix.to_lowercase(),
                    PrefixRoute {
                        handler_id: id.clone(),
                        transform: trigger.transform.clone(),
                    },
                );
            }
        }
        self.handlers.insert(id, handler);
    }

    /// Is `word` a registered keyword prefix? Used by the router (and typo
    /// suggestions) to know a word is a real command.
    pub fn is_known_prefix(&self, word: &str) -> bool {
        self.prefix_index.contains_key(&word.to_lowercase())
    }

    /// Route a keyword prefix: returns `(handler_id, transformed_args)` if the
    /// first word is a registered trigger. `rest` is the trimmed text after the
    /// keyword. Returns `None` for unknown prefixes (caller falls through to
    /// structural detection / AI).
    pub fn route_prefix(&self, keyword: &str, rest: &str) -> Option<(String, String)> {
        let route = self.prefix_index.get(&keyword.to_lowercase())?;
        Some((
            route.handler_id.clone(),
            route.transform.apply(keyword, rest),
        ))
    }

    /// All registered keyword prefixes (for diagnostics / help).
    pub fn known_prefixes(&self) -> Vec<&str> {
        self.prefix_index.keys().map(|s| s.as_str()).collect()
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

    /// Build a user-facing command catalog from the live registry — the dynamic
    /// help/guide source. One entry per handler that has at least one keyword
    /// trigger (structural-only handlers with no typable keyword are skipped),
    /// carrying its primary keyword + description. Sorted by keyword.
    ///
    /// This never goes stale: registering a new handler makes it appear here
    /// automatically, so the Guide is generated, not hand-maintained.
    pub fn command_catalog(&self) -> Vec<CommandInfo> {
        let mut items: Vec<CommandInfo> = self
            .handlers
            .values()
            .filter_map(|h| {
                // Primary keyword = first prefix of the first trigger. Handlers
                // reached only structurally or via AI (no triggers) are omitted.
                let keyword = h
                    .triggers()
                    .iter()
                    .flat_map(|t| t.prefixes.iter().copied())
                    .next()?;
                Some(CommandInfo {
                    id: h.id().to_string(),
                    keyword: keyword.to_string(),
                    description: h.description().to_string(),
                })
            })
            .collect();
        items.sort_by(|a, b| a.keyword.cmp(&b.keyword));
        items
    }

    /// Build the dynamic Triggers list for the Guide. Structural sigils (`=`,
    /// `>`, `~/`, …) carry fixed descriptions; shorthand colon-triggers (`e:`,
    /// `w:`, …) pull their description from the SAME live handler the command
    /// list uses — one source of truth, so a trigger and its command never drift.
    /// Colon-triggers whose handler is absent are skipped.
    pub fn trigger_catalog(&self) -> Vec<CommandInfo> {
        use crate::intent::patterns::{COLON_TRIGGERS, SIGIL_TRIGGERS};

        let mut items: Vec<CommandInfo> = Vec::new();

        // Structural sigils first (in declared order — most common at top).
        for &(sigil, desc) in SIGIL_TRIGGERS {
            items.push(CommandInfo {
                id: String::new(),
                keyword: sigil.to_string(),
                description: desc.to_string(),
            });
        }

        // Colon shorthands, description sourced from the live handler.
        let mut colon: Vec<CommandInfo> = COLON_TRIGGERS
            .iter()
            .filter_map(|&(prefix, handler_id)| {
                let h = self.handlers.get(handler_id)?;
                Some(CommandInfo {
                    id: handler_id.to_string(),
                    keyword: prefix.to_string(),
                    description: h.description().to_string(),
                })
            })
            .collect();
        colon.sort_by(|a, b| a.keyword.cmp(&b.keyword));
        items.extend(colon);
        items
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
    use crate::action_registry::{ActionHandler, ActionResult, OutputType, RiskLevel};
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
        async fn execute(
            &self,
            _ctx: &crate::action_registry::ExecContext,
            args: &str,
        ) -> Result<ActionResult, LychiError> {
            Ok(ActionResult::ok(
                format!("executed with: {args}"),
                OutputType::Status,
            ))
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

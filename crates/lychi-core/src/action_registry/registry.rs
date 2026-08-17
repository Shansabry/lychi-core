use std::collections::HashMap;

use crate::action_registry::grammar::{self, ToolGroup};
use crate::action_registry::trigger::ArgTransform;
use crate::action_registry::{ActionHandler, CommandCategory, CommandInfo, CompletionItem};

/// One model-facing tool as projected by [`ActionRegistry::model_catalog`]:
/// either a group tool fronting several grammared handlers, or a standalone
/// handler exposed as before.
#[derive(Clone, Debug)]
pub struct ModelTool {
    pub name: String,
    pub description: String,
    pub input_schema: Option<serde_json::Value>,
    /// Standalone tools: whether the whole tool mutates state.
    pub mutates: bool,
    /// Group tools: the compound actions that mutate state (empty for
    /// standalone tools — use `mutates`).
    pub mutating_actions: Vec<String>,
}

/// The outcome of [`ActionRegistry::resolve_group_call`].
#[derive(Debug)]
pub enum GroupDispatch {
    /// Not a group tool — dispatch the name to the executor unchanged.
    NotAGroup,
    /// A group call resolved to its member handler and flat args.
    Resolved {
        handler_id: String,
        flat_args: String,
    },
    /// A group call that cannot run (bad JSON, unknown action, missing
    /// required operand). The message is the model-facing error tool_result.
    Invalid(String),
}

/// Build a `CommandInfo`, denormalising the category's title + order so the
/// frontend groups without needing its own category table.
#[allow(clippy::too_many_arguments)]
fn command_info(
    id: &str,
    keyword: &str,
    description: &str,
    usage: &str,
    input_schema: Option<serde_json::Value>,
    category: CommandCategory,
    mutates: bool,
) -> CommandInfo {
    CommandInfo {
        id: id.to_string(),
        keyword: keyword.to_string(),
        description: description.to_string(),
        usage: usage.to_string(),
        input_schema,
        category,
        category_title: category.title().to_string(),
        category_order: category.order(),
        mutates,
    }
}

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
///
/// `Clone` is part of the contract: handlers are stored as `Arc`, so a clone is
/// a cheap map-of-refcounts copy sharing the same handler instances. This is
/// what lets the app snapshot the whole `Executor` and run a command WITHOUT
/// holding the executor lock across the handler's execution — the freeze class
/// where one slow handler plus one config save (a queued `blocking_write` on a
/// fair RwLock) stalled every subsequent keystroke's completions.
#[derive(Clone)]
pub struct ActionRegistry {
    handlers: HashMap<String, std::sync::Arc<dyn ActionHandler>>,
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
        // Stored as Arc (from the Box every call site already passes) so the
        // registry stays cheaply clonable — see the struct doc.
        let handler: std::sync::Arc<dyn ActionHandler> = std::sync::Arc::from(handler);
        // Index this handler's keyword triggers. Registering a handler with an
        // id that already exists (hot-reload of shell/quicklink/project handlers)
        // replaces both the handler and its prefix routes.
        let id = handler.id().to_string();
        // Drop any stale routes that pointed at this id (in case its triggers changed).
        self.prefix_index.retain(|_, r| r.handler_id != id);
        for trigger in handler.triggers() {
            for prefix in trigger.prefixes {
                let previous = self.prefix_index.insert(
                    prefix.to_lowercase(),
                    PrefixRoute {
                        handler_id: id.clone(),
                        transform: trigger.transform.clone(),
                    },
                );
                // The retain() above already dropped this id's own stale
                // routes, so a surviving previous owner is a DIFFERENT handler
                // losing its keyword to registration order. Warn rather than
                // assert: quicklinks and script commands register user-chosen
                // keywords through this same path, and a user naming a
                // quicklink "net" must get a diagnosable log line, not a
                // panic. The built-in set is held collision-free by the
                // registry test in state.rs building the real production set.
                if let Some(prev) = previous
                    && prev.handler_id != id
                {
                    tracing::warn!(
                        "[registry] prefix '{prefix}': '{id}' steals the route from '{}' \
                         (registration order decides — rename one of them)",
                        prev.handler_id
                    );
                }
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

    /// All registered keyword prefixes (for diagnostics / help), sorted.
    ///
    /// Also serves the Settings UI's collision warning, so the frontend does not
    /// keep its own copy of the list — a hand-maintained duplicate is a second
    /// decider that drifts silently as handlers are added. Sorted so the order
    /// is stable across runs (`HashMap` iteration order is arbitrary).
    pub fn known_prefixes(&self) -> Vec<&str> {
        let mut out: Vec<&str> = self.prefix_index.keys().map(|s| s.as_str()).collect();
        out.sort_unstable();
        out
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
                Some(command_info(
                    h.id(),
                    keyword,
                    h.description(),
                    h.usage(),
                    h.input_schema(),
                    h.category(),
                    h.mutates_state(),
                ))
            })
            .collect();
        // Group by category order, then alphabetise within each group.
        items.sort_by(|a, b| {
            a.category_order
                .cmp(&b.category_order)
                .then_with(|| a.keyword.cmp(&b.keyword))
        });
        items
    }

    /// The MODEL-facing tool catalog: grammared handlers fold into their
    /// [`ToolGroup`]'s single tool (compound actions, merged operands,
    /// per-action mutation list); everything else stays a standalone tool as
    /// before. Deterministic order — groups in `ToolGroup::ALL` order, then
    /// standalone tools alphabetically — because a byte-stable tool block is
    /// what provider prompt caching keys on.
    ///
    /// A standalone tool's description folds in its `usage()` text: with no
    /// prose manifest sent, the wire description is the only channel the model
    /// gets. Grouped tools don't need the fold — their knowledge is in the
    /// generated action list and operand descriptions.
    pub fn model_catalog(&self) -> Vec<ModelTool> {
        let mut groups: HashMap<ToolGroup, Vec<(String, grammar::Verb)>> = HashMap::new();
        let mut standalone: Vec<ModelTool> = Vec::new();

        let mut handlers: Vec<&std::sync::Arc<dyn ActionHandler>> = self
            .handlers
            .values()
            // Same visibility rule as `command_catalog`: keyword-less handlers
            // (structural/internal) are not model tools either.
            .filter(|h| {
                h.triggers()
                    .iter()
                    .any(|t| !t.prefixes.is_empty())
            })
            .collect();
        handlers.sort_by_key(|h| h.id());

        for h in handlers {
            match (h.grammar(), h.tool_group()) {
                (Some(g), group) if group != ToolGroup::Standalone => {
                    let bucket = groups.entry(group).or_default();
                    for v in g.verbs {
                        bucket.push((grammar::compound_action(h.id(), v), *v));
                    }
                }
                _ => {
                    let description = if h.usage().trim().is_empty() {
                        h.description().to_string()
                    } else {
                        format!(
                            "{}. Usage: {}",
                            h.description().trim_end_matches('.'),
                            h.usage().trim()
                        )
                    };
                    standalone.push(ModelTool {
                        name: h.id().to_string(),
                        description,
                        input_schema: h.input_schema(),
                        mutates: h.mutates_state(),
                        mutating_actions: Vec::new(),
                    });
                }
            }
        }

        let mut out: Vec<ModelTool> = Vec::new();
        for &group in ToolGroup::ALL {
            let Some(actions) = groups.remove(&group) else {
                continue;
            };
            let mutating_actions: Vec<String> = actions
                .iter()
                .filter(|(_, v)| v.mutates)
                .map(|(n, _)| n.clone())
                .collect();
            out.push(ModelTool {
                name: group.tool_name().to_string(),
                description: group.description().to_string(),
                input_schema: Some(grammar::group_schema(&actions)),
                mutates: false, // per-action via mutating_actions
                mutating_actions,
            });
        }
        out.extend(standalone);
        out
    }

    /// Resolve a model tool call against the group projection: a group call
    /// becomes its member handler + the flat args that handler's parser (and,
    /// upstream of execution, the Rules Engine) understands. Anything that is
    /// not a group tool passes through untouched — the executor dispatches
    /// handler ids exactly as before, so the model calling a handler by name
    /// (stale hint, old conversation) still works.
    pub fn resolve_group_call(&self, tool: &str, args: &str) -> GroupDispatch {
        let Some(group) = ToolGroup::ALL
            .iter()
            .copied()
            .find(|g| g.tool_name() == tool)
        else {
            return GroupDispatch::NotAGroup;
        };

        let parsed: serde_json::Value = match serde_json::from_str(args.trim()) {
            Ok(v) => v,
            Err(_) => {
                return GroupDispatch::Invalid(format!(
                    "`{tool}` takes a JSON object with an `action` field, e.g. \
                     {{\"action\": \"…\"}} — got a non-JSON argument."
                ));
            }
        };
        let Some(map) = parsed.as_object() else {
            return GroupDispatch::Invalid(format!(
                "`{tool}` takes a JSON object with an `action` field."
            ));
        };
        let Some(action) = map.get("action").and_then(|a| a.as_str()) else {
            return GroupDispatch::Invalid(
                "Missing required `action` field — pick one of this tool's listed actions."
                    .to_string(),
            );
        };

        // Find the member handler whose compound action this is. Scan members
        // rather than split the string: handler ids may contain underscores.
        for h in self.handlers.values() {
            if h.tool_group() != group {
                continue;
            }
            let Some(g) = h.grammar() else { continue };
            for v in g.verbs {
                if grammar::compound_action(h.id(), v) != action {
                    continue;
                }
                // Required-operand check here, where the field names are known
                // — the flat parser's usage error mentions flat syntax the
                // model never sees.
                let missing: Vec<&str> = v
                    .operands
                    .iter()
                    .filter(|op| op.required)
                    .filter(|op| {
                        !map.get(op.name).is_some_and(|val| match val {
                            serde_json::Value::String(s) => !s.trim().is_empty(),
                            serde_json::Value::Array(a) => !a.is_empty(),
                            serde_json::Value::Null => false,
                            _ => true,
                        })
                    })
                    .map(|op| op.name)
                    .collect();
                if !missing.is_empty() {
                    return GroupDispatch::Invalid(format!(
                        "`{action}` requires: {}.",
                        missing.join(", ")
                    ));
                }
                return GroupDispatch::Resolved {
                    handler_id: h.id().to_string(),
                    flat_args: g.to_flat(v, map),
                };
            }
        }
        GroupDispatch::Invalid(format!(
            "Unknown action `{action}` for `{tool}` — pick one of the actions listed in \
             the tool's schema."
        ))
    }

    /// Build the dynamic Triggers list for the Guide. Structural sigils (`=`,
    /// `>`, `~/`, …) carry fixed descriptions; shorthand colon-triggers (`e:`,
    /// `w:`, …) pull their description from the SAME live handler the command
    /// list uses — one source of truth, so a trigger and its command never drift.
    /// Colon-triggers whose handler is absent are skipped.
    pub fn trigger_catalog(&self) -> Vec<CommandInfo> {
        // From the leaf `triggers` module — NOT `intent`, which would be an
        // upward action_registry → intent edge (EXEC-7).
        use crate::triggers::{COLON_TRIGGERS, SIGIL_TRIGGERS};

        let mut items: Vec<CommandInfo> = Vec::new();

        // Structural sigils first (in declared order — most common at top).
        for &(sigil, desc) in SIGIL_TRIGGERS {
            items.push(command_info(
                "",
                sigil,
                desc,
                "",
                None,
                CommandCategory::General,
                false,
            ));
        }

        // Colon shorthands, description + category sourced from the live handler.
        let mut colon: Vec<CommandInfo> = COLON_TRIGGERS
            .iter()
            .filter_map(|&(prefix, handler_id)| {
                let h = self.handlers.get(handler_id)?;
                Some(command_info(
                    handler_id,
                    prefix,
                    h.description(),
                    "",
                    None,
                    h.category(),
                    h.mutates_state(),
                ))
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

    // ── Model catalog projection ─────────────────────────────────────────────

    use crate::action_registry::grammar::{ArgKind, Grammar, Operand, Verb};
    use crate::action_registry::trigger::Trigger;

    /// A grammared handler in the Personal group: `note` with add/read.
    struct GNote;
    #[async_trait]
    impl ActionHandler for GNote {
        fn id(&self) -> &str {
            "note"
        }
        fn description(&self) -> &str {
            "Notes"
        }
        fn triggers(&self) -> &'static [Trigger] {
            const T: &[Trigger] = &[Trigger::new(&["note"], ArgTransform::PassThrough)];
            T
        }
        fn tool_group(&self) -> ToolGroup {
            ToolGroup::Personal
        }
        fn grammar(&self) -> Option<Grammar> {
            Some(Grammar {
                verbs: &[
                    Verb {
                        name: "add",
                        desc: "Add a note",
                        mutates: true,
                        operands: &[Operand {
                            name: "text",
                            desc: "The note text",
                            required: true,
                            kind: ArgKind::Text,
                            prefix: None,
                        }],
                    },
                    Verb {
                        name: "read",
                        desc: "List notes",
                        mutates: false,
                        operands: &[],
                    },
                ],
            })
        }
        async fn execute(
            &self,
            _ctx: &crate::action_registry::ExecContext,
            args: &str,
        ) -> Result<ActionResult, LychiError> {
            Ok(ActionResult::ok(
                format!("note: {args}"),
                OutputType::Status,
            ))
        }
    }

    /// A free-form grammared handler in the same group.
    struct GSnip;
    #[async_trait]
    impl ActionHandler for GSnip {
        fn id(&self) -> &str {
            "snip"
        }
        fn description(&self) -> &str {
            "Snippets"
        }
        fn triggers(&self) -> &'static [Trigger] {
            const T: &[Trigger] = &[Trigger::new(&["snip"], ArgTransform::PassThrough)];
            T
        }
        fn tool_group(&self) -> ToolGroup {
            ToolGroup::Personal
        }
        fn grammar(&self) -> Option<Grammar> {
            Some(Grammar {
                verbs: &[Verb {
                    name: "",
                    desc: "Search snippets",
                    mutates: false,
                    operands: &[Operand {
                        name: "query",
                        desc: "Search text",
                        required: false,
                        kind: ArgKind::Text,
                        prefix: None,
                    }],
                }],
            })
        }
        async fn execute(
            &self,
            _ctx: &crate::action_registry::ExecContext,
            args: &str,
        ) -> Result<ActionResult, LychiError> {
            Ok(ActionResult::ok(
                format!("snip: {args}"),
                OutputType::Status,
            ))
        }
    }

    /// A keyword-bearing handler with no grammar — stays standalone.
    struct GPlain;
    #[async_trait]
    impl ActionHandler for GPlain {
        fn id(&self) -> &str {
            "plain"
        }
        fn description(&self) -> &str {
            "Plain tool"
        }
        fn usage(&self) -> &str {
            "plain <thing>"
        }
        fn triggers(&self) -> &'static [Trigger] {
            const T: &[Trigger] = &[Trigger::new(&["plain"], ArgTransform::PassThrough)];
            T
        }
        async fn execute(
            &self,
            _ctx: &crate::action_registry::ExecContext,
            args: &str,
        ) -> Result<ActionResult, LychiError> {
            Ok(ActionResult::ok(
                format!("plain: {args}"),
                OutputType::Status,
            ))
        }
    }

    fn projected_registry() -> ActionRegistry {
        let mut r = ActionRegistry::new();
        r.register(Box::new(GNote));
        r.register(Box::new(GSnip));
        r.register(Box::new(GPlain));
        r
    }

    #[test]
    fn grammared_handlers_fold_into_one_group_tool() {
        let cat = projected_registry().model_catalog();
        let personal = cat.iter().find(|t| t.name == "personal_data").unwrap();
        let schema = personal.input_schema.as_ref().unwrap();
        let actions = schema["properties"]["action"]["enum"].as_array().unwrap();
        let names: Vec<&str> = actions.iter().filter_map(|v| v.as_str()).collect();
        assert_eq!(names, vec!["note_add", "note_read", "snip"]);
        // Mutation is per action, not per tool.
        assert!(!personal.mutates);
        assert_eq!(personal.mutating_actions, vec!["note_add".to_string()]);
        // The grammarless handler stays standalone, usage folded in.
        let plain = cat.iter().find(|t| t.name == "plain").unwrap();
        assert!(plain.description.contains("Usage: plain <thing>"));
        // Group tools come first, so the tool block is stable as handlers churn.
        assert!(
            cat.iter().position(|t| t.name == "personal_data").unwrap()
                < cat.iter().position(|t| t.name == "plain").unwrap()
        );
    }

    #[test]
    fn group_call_resolves_to_handler_and_flat_args() {
        let r = projected_registry();
        match r.resolve_group_call(
            "personal_data",
            r#"{"action":"note_add","text":"buy milk"}"#,
        ) {
            GroupDispatch::Resolved {
                handler_id,
                flat_args,
            } => {
                assert_eq!(handler_id, "note");
                assert_eq!(flat_args, "add buy milk");
            }
            other => panic!("expected Resolved, got {other:?}"),
        }
        // Free-form member: action is the handler id, flat is just the operands.
        match r.resolve_group_call("personal_data", r#"{"action":"snip","query":"ssh"}"#) {
            GroupDispatch::Resolved {
                handler_id,
                flat_args,
            } => {
                assert_eq!(handler_id, "snip");
                assert_eq!(flat_args, "ssh");
            }
            other => panic!("expected Resolved, got {other:?}"),
        }
    }

    #[test]
    fn group_call_rejects_missing_required_and_unknown_action() {
        let r = projected_registry();
        match r.resolve_group_call("personal_data", r#"{"action":"note_add"}"#) {
            GroupDispatch::Invalid(msg) => assert!(msg.contains("text"), "{msg}"),
            other => panic!("expected Invalid, got {other:?}"),
        }
        match r.resolve_group_call("personal_data", r#"{"action":"nope"}"#) {
            GroupDispatch::Invalid(msg) => assert!(msg.contains("nope"), "{msg}"),
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn non_group_names_pass_through() {
        let r = projected_registry();
        assert!(matches!(
            r.resolve_group_call("note", "add hi"),
            GroupDispatch::NotAGroup
        ));
        assert!(matches!(
            r.resolve_group_call("run", "ls"),
            GroupDispatch::NotAGroup
        ));
    }

    #[test]
    fn list_ids() {
        let mut registry = ActionRegistry::new();
        registry.register(Box::new(DummyHandler));
        let ids = registry.list_ids();
        assert!(ids.contains(&"test"));
    }
}

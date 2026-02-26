//! Context debug handler — shows all detected environment signals.
//!
//! Usage: `ctx` — displays active window, CWD, git, project, docker context
//! with gather latency. Power user tool for transparency.

use std::sync::Mutex;

use async_trait::async_trait;

use crate::action_registry::{ActionHandler, ActionResult, CompletionItem, OutputType};
use crate::context::EnvironmentContext;
use crate::error::LychiError;

/// Snapshot of the current context, set by the executor before execute().
static CONTEXT_SNAPSHOT: Mutex<Option<EnvironmentContext>> = Mutex::new(None);

/// Set the context snapshot for the next `ctx` execution.
pub fn set_context(ctx: Option<EnvironmentContext>) {
    if let Ok(mut guard) = CONTEXT_SNAPSHOT.lock() {
        *guard = ctx;
    }
}

pub struct ContextDebugHandler;

impl Default for ContextDebugHandler {
    fn default() -> Self {
        Self::new()
    }
}

impl ContextDebugHandler {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ActionHandler for ContextDebugHandler {
    fn id(&self) -> &str {
        "ctx"
    }

    fn description(&self) -> &str {
        "Show current environment context (debug)"
    }

    async fn execute(&self, _args: &str) -> Result<ActionResult, LychiError> {
        let ctx = CONTEXT_SNAPSHOT.lock().ok().and_then(|g| g.clone());

        let output = match ctx {
            None => "No context gathered yet.".to_string(),
            Some(c) => format_context(&c),
        };

        Ok(ActionResult::ok(output, OutputType::Text))
    }

    async fn completions(&self, _partial: &str) -> Vec<CompletionItem> {
        vec![CompletionItem {
            label: "ctx".to_string(),
            icon_path: Some("__context__".to_string()),
            score: 100,
            description: Some("Show environment context signals".to_string()),
            reason: None,
        }]
    }
}

fn format_context(ctx: &EnvironmentContext) -> String {
    let mut lines = vec![format!("Context gathered in {}ms", ctx.gather_ms)];
    lines.push(String::new());

    // Window
    match &ctx.active_window {
        Some(w) => lines.push(format!(
            "Window: {} ({}) pid={} terminal={} ide={}",
            w.title, w.wm_class, w.pid, w.is_terminal, w.is_ide
        )),
        None => lines.push("Window: none".to_string()),
    }

    // CWD
    match &ctx.cwd {
        Some(cwd) => lines.push(format!("CWD: {cwd}")),
        None => lines.push("CWD: none".to_string()),
    }

    // Terminal CWD (from window stack — set when IDE has focus)
    match &ctx.terminal_cwd {
        Some(tcwd) => lines.push(format!("Terminal CWD: {tcwd}")),
        None => lines.push("Terminal CWD: none".to_string()),
    }

    // Git
    match &ctx.git {
        Some(g) => {
            let remote = g
                .remote
                .as_deref()
                .map(|r| format!(" remote={r}"))
                .unwrap_or_default();
            lines.push(format!(
                "Git: branch={} dirty={} root={}{remote}",
                g.branch, g.dirty, g.repo_root
            ));
        }
        None => lines.push("Git: none".to_string()),
    }

    // Project
    match &ctx.project {
        Some(p) => {
            let scripts_str = if p.scripts.is_empty() {
                String::new()
            } else {
                let names: Vec<&str> = p.scripts.iter().map(|s| s.name.as_str()).collect();
                format!(" scripts=[{}]", names.join(", "))
            };
            let pm_str = p
                .package_manager
                .as_deref()
                .map(|pm| format!(" pkg_manager={pm}"))
                .unwrap_or_default();
            lines.push(format!(
                "Project: {:?} root={} compose={}{pm_str}{scripts_str}",
                p.kind, p.root, p.has_compose
            ));
        }
        None => lines.push("Project: none".to_string()),
    }

    // Docker
    match &ctx.docker {
        Some(d) => {
            lines.push(format!("Docker: containers={}", d.containers.len()));
            for c in &d.containers {
                lines.push(format!("  {} ({}) — {}", c.name, c.image, c.status));
            }
        }
        None => lines.push("Docker: none".to_string()),
    }

    // Terminal
    match &ctx.terminal_class {
        Some(tc) => lines.push(format!("Terminal: {tc}")),
        None => lines.push("Terminal: none".to_string()),
    }

    // Time
    lines.push(format!("Hour: {}", ctx.hour));

    // Cache
    let cache_stats = crate::context::cache::stats();
    let fmt = |ms: Option<u64>, inv: Option<crate::context::cache::InvalidationReason>| {
        let age = match ms {
            Some(age) => format!("{age}ms ago"),
            None => "empty".to_string(),
        };
        match inv {
            Some(reason) => format!("{age} / last miss: {}", reason.as_str()),
            None => age,
        }
    };
    lines.push(format!(
        "Cache: git={}, docker={}, project={}",
        fmt(cache_stats.git_age_ms, cache_stats.git_invalidation),
        fmt(cache_stats.docker_age_ms, cache_stats.docker_invalidation),
        fmt(cache_stats.project_age_ms, cache_stats.project_invalidation),
    ));

    // Suggestions with provenance
    let suggestions = crate::context::suggestions::suggest(ctx);
    if !suggestions.is_empty() {
        lines.push(String::new());
        lines.push(format!("Suggestions: ({})", suggestions.len()));
        for item in suggestions.iter().take(10) {
            let reason = item.reason.as_deref().unwrap_or("?");
            lines.push(format!("  {} — {}", item.label, reason));
        }
    }

    lines.join("\n")
}

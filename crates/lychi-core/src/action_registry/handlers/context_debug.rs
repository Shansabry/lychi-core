//! Context debug handler — shows all detected environment signals.
//!
//! Usage: `ctx` — displays active window, CWD, git, project, docker context
//! with gather latency. Power user tool for transparency.

use std::sync::Mutex;

use async_trait::async_trait;

use crate::action_registry::grammar::{ArgKind, Grammar, Operand, ToolGroup, Verb};
use crate::action_registry::{
    ActionHandler, ActionResult, CommandCategory, CompletionItem, ExecContext, OutputType,
};
use crate::context::EnvironmentContext;
use crate::error::LychiError;

/// `ctx`'s argument surface: a single free-form action whose flat forms are
/// exactly the four spellings the string-matching below accepts — "",
/// "metrics", "metrics --reset", "metrics --rate". A Choice operand renders
/// its value verbatim, so the mode values ARE the flag spellings; the JSON
/// Schema constrains a model to them, and the structured→flat adapter derives
/// from this. Read-only diagnostics: `--reset` only rebases an in-process
/// counter baseline — no file, system, or stored data changes.
const CTX_GRAMMAR: Grammar = Grammar {
    verbs: &[Verb {
        name: "",
        desc: "Inspect the launcher's own context-awareness state — read-only \
               diagnostics for debugging why a suggestion or routing decision \
               happened. Returns either the full snapshot of detected environment \
               signals or the context system's internal counters; never needed \
               for ordinary user tasks.",
        mutates: false,
        operands: &[
            Operand {
                name: "topic",
                desc: "Omit entirely to dump the full context snapshot: active \
                       window, cwd, git, project, docker containers, terminal, \
                       clipboard type, network, cache ages, focus ring, and the \
                       derived suggestions. Pass \"metrics\" for the context \
                       system's process-lifetime counters instead.",
                required: false,
                kind: ArgKind::Choice(&["metrics"]),
                prefix: None,
            },
            Operand {
                name: "mode",
                desc: "Only meaningful with topic \"metrics\": \"--reset\" sets a \
                       new baseline for delta tracking; \"--rate\" reports counter \
                       deltas and per-second rates since that baseline. Omit for \
                       lifetime totals.",
                required: false,
                kind: ArgKind::Choice(&["--reset", "--rate"]),
                prefix: None,
            },
        ],
    }],
};

/// Normalize the tool's `args` to the flat string the matcher below reads. A
/// constrained model sends the structured JSON (`{"topic":"metrics"}`); a
/// human or legacy/flat caller sends the string directly and passes through
/// unchanged. Malformed JSON falls back to the raw string.
fn ctx_args_to_flat(args: &str) -> String {
    CTX_GRAMMAR
        .flatten_json(args)
        .unwrap_or_else(|| args.trim().to_string())
}

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
    fn triggers(&self) -> &'static [crate::action_registry::Trigger] {
        use crate::action_registry::Trigger;
        static TRIGGERS: &[Trigger] = &[Trigger::keywords(&["ctx"])];
        TRIGGERS
    }

    fn id(&self) -> &str {
        "ctx"
    }

    fn description(&self) -> &str {
        "Show current environment context (debug)"
    }
    fn category(&self) -> CommandCategory {
        CommandCategory::Developer
    }
    fn grammar(&self) -> Option<Grammar> {
        Some(CTX_GRAMMAR)
    }
    fn tool_group(&self) -> ToolGroup {
        ToolGroup::Dev
    }

    async fn execute(&self, _ctx: &ExecContext, args: &str) -> Result<ActionResult, LychiError> {
        // A constrained model sends `{"topic":..,"mode":..}`; flatten it (a
        // plain-string caller passes through) to the spellings matched below.
        let flat = ctx_args_to_flat(args);
        let args_trimmed = flat.trim();

        // `ctx metrics` — process-lifetime totals
        if args_trimmed == "metrics" {
            let m = crate::context::metrics::snapshot();
            let output = format!(
                "Context metrics (process lifetime totals)\n\
                 \n\
                 Freshness\n\
                 hard_stale_hit:               {}\n\
                 soft_stale_hit:               {}\n\
                 stale_refresh_triggered:      {}\n\
                 \n\
                 Correctness guardrails\n\
                 terminal_incoherent_filtered: {}\n\
                 \n\
                 Intent binding\n\
                 clipboard_expansion_used:     {}\n\
                 clipboard_expansion_miss:     {} (empty={}, type_mismatch={})\n\
                 \n\
                 Terminal routing\n\
                 terminal_route_hit:           {}\n\
                 terminal_route_busy:          {}\n\
                 terminal_route_fail:          {}\n\
                 terminal_route_no_protocol:   {}",
                m.hard_stale_hit,
                m.soft_stale_hit,
                m.stale_refresh_triggered,
                m.terminal_incoherent_filtered,
                m.clipboard_expansion_used,
                m.clipboard_expansion_miss(),
                m.clipboard_expansion_miss_empty,
                m.clipboard_expansion_miss_type,
                m.terminal_route_hit,
                m.terminal_route_busy,
                m.terminal_route_fail,
                m.terminal_route_no_protocol,
            );
            return Ok(ActionResult::ok(output, OutputType::Text));
        }

        // `ctx metrics --reset` — set baseline for delta tracking
        if args_trimmed == "metrics --reset" {
            crate::context::metrics::reset_baseline();
            return Ok(ActionResult::ok(
                "Baseline reset. Use 'ctx metrics --rate' to see deltas.".to_string(),
                OutputType::Text,
            ));
        }

        // `ctx metrics --rate` — show deltas since last reset
        if args_trimmed.starts_with("metrics --rate") {
            return Ok(ActionResult::ok(format_metrics_rate(), OutputType::Text));
        }

        let ctx = CONTEXT_SNAPSHOT.lock().ok().and_then(|g| g.clone());

        let output = match ctx {
            None => "No context gathered yet.".to_string(),
            Some(c) => format_context(&c),
        };

        Ok(ActionResult::ok(output, OutputType::Text))
    }

    async fn completions(&self, partial: &str) -> Vec<CompletionItem> {
        let mut items = vec![CompletionItem {
            label: "ctx".to_string(),
            icon_path: Some("__context__".to_string()),
            score: 100,
            description: Some("Show environment context signals".to_string()),
            reason: None,
            thumb_b64: None,
            run: Some("ctx".to_string()),
            ..Default::default()
        }];
        let p = partial.trim();
        if "metrics".starts_with(p) || p.starts_with("metrics") {
            items.push(CompletionItem {
                label: "ctx metrics".to_string(),
                icon_path: Some("__context__".to_string()),
                score: 90,
                description: Some(
                    "Show context system counters (staleness, coherence, clipboard)".to_string(),
                ),
                reason: None,
                thumb_b64: None,
                run: Some("ctx metrics".to_string()),
                ..Default::default()
            });
            items.push(CompletionItem {
                label: "ctx metrics --reset".to_string(),
                icon_path: Some("__context__".to_string()),
                score: 85,
                description: Some("Set baseline for delta/rate reporting".to_string()),
                reason: None,
                thumb_b64: None,
                run: Some("ctx metrics --reset".to_string()),
                ..Default::default()
            });
            items.push(CompletionItem {
                label: "ctx metrics --rate".to_string(),
                icon_path: Some("__context__".to_string()),
                score: 84,
                description: Some("Show counter deltas since last reset".to_string()),
                reason: None,
                thumb_b64: None,
                run: Some("ctx metrics --rate".to_string()),
                ..Default::default()
            });
        }
        items
    }
}

fn format_metrics_rate() -> String {
    match crate::context::metrics::rate_since_baseline() {
        None => "No baseline set. Run 'ctx metrics --reset' first.".to_string(),
        Some((delta, elapsed_secs, baseline_at)) => {
            // Format the baseline wall-clock time from the Instant by converting elapsed to SystemTime.
            let baseline_time = {
                let elapsed = baseline_at.elapsed();
                std::time::SystemTime::now()
                    .checked_sub(elapsed)
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| {
                        let secs = d.as_secs();
                        let h = (secs % 86400) / 3600;
                        let m = (secs % 3600) / 60;
                        let s = secs % 60;
                        format!("{h:02}:{m:02}:{s:02}")
                    })
                    .unwrap_or_else(|| "unknown".to_string())
            };
            let r = |n: u64| -> f64 {
                if elapsed_secs > 0.0 {
                    n as f64 / elapsed_secs
                } else {
                    0.0
                }
            };
            let rate = |n: u64| {
                if elapsed_secs > 0.0 {
                    format!("{n:>3}  ({:.2}/s)", r(n))
                } else {
                    format!("{n:>3}  (--/s)")
                }
            };

            // ── Health summary ──────────────────────────────────────────────
            let hard_stale_r = r(delta.hard_stale_hit);
            let soft_stale_r = r(delta.soft_stale_hit);
            let refresh_r = r(delta.stale_refresh_triggered);
            let incoherent_r = r(delta.terminal_incoherent_filtered);
            let clip_miss_r = r(delta.clipboard_expansion_miss());

            // Health classification: any hard-stale is always "stale" — no downgrading to "degraded".
            let (health_icon, health_reason, health_action) = if hard_stale_r > 0.0 {
                (
                    "⚠  stale",
                    format!(
                        "hard_stale_hit firing ({:.2}/s) — context expiring mid-session",
                        hard_stale_r
                    ),
                    Some(
                        "ensure background re-gather fires on idle→interaction (summon after long pause)",
                    ),
                )
            } else if soft_stale_r > 0.1 && refresh_r < soft_stale_r {
                (
                    "⚠  degraded",
                    format!(
                        "soft-stale ({:.2}/s) but refresh not keeping up ({:.2}/s)",
                        soft_stale_r, refresh_r
                    ),
                    Some("increase refresh trigger frequency or lower SOFT_STALE_SECS threshold"),
                )
            } else if incoherent_r > 0.2 {
                (
                    "⚠  degraded",
                    format!(
                        "terminal_incoherent_filtered spiking ({:.2}/s) — multi-project noise",
                        incoherent_r
                    ),
                    Some(
                        "review terminal_matches_workspace logic; consider terminal focus ring (3.1i)",
                    ),
                )
            } else {
                let parts: Vec<String> = [
                    if hard_stale_r == 0.0 {
                        Some("no hard-stale".to_string())
                    } else {
                        None
                    },
                    Some(format!("incoherence {:.2}/s", incoherent_r)),
                    Some(format!("soft-stale {:.2}/s", soft_stale_r)),
                ]
                .into_iter()
                .flatten()
                .collect();
                ("✓  stable", parts.join("; "), None)
            };
            let action_line = health_action
                .map(|a| format!("\nSuggested action: {a}"))
                .unwrap_or_default();

            // Refresh coverage — only when < 90% (not interesting otherwise).
            let refresh_coverage = if soft_stale_r > 0.1 {
                let pct = (refresh_r / soft_stale_r * 100.0) as u64;
                if pct < 90 {
                    format!(
                        "\nRefresh coverage: {}% (triggered {:.2}/s ÷ soft-stale {:.2}/s)",
                        pct, refresh_r, soft_stale_r
                    )
                } else {
                    String::new()
                }
            } else {
                String::new()
            };

            // Tip line — only when unhealthy, guides the next debugging step.
            let tip_line = if health_action.is_some() {
                "\nTip: run 'ctx metrics --reset' then reproduce to measure improvements"
            } else {
                ""
            };

            // Top issue: priority-ranked, not just max rate. Good-news counters
            // (stale_refresh_triggered, clipboard_expansion_used) never surface as issues.
            struct Issue {
                label: &'static str,
                count: u64,
                rate: f64,
            }
            let issues: Vec<Issue> = vec![
                Issue {
                    label: "hard_stale_hit (context expiring mid-session)",
                    count: delta.hard_stale_hit,
                    rate: hard_stale_r,
                },
                Issue {
                    label: "terminal_incoherent_filtered (multi-project noise)",
                    count: delta.terminal_incoherent_filtered,
                    rate: incoherent_r,
                },
                Issue {
                    label: "soft_stale_hit (refresh cadence may be too slow)",
                    count: delta.soft_stale_hit,
                    rate: soft_stale_r,
                },
                Issue {
                    label: "clipboard_expansion_miss (implicit verb, no match)",
                    count: delta.clipboard_expansion_miss(),
                    rate: clip_miss_r,
                },
            ];
            // First non-zero entry in priority order is the top issue.
            let top_issue = issues
                .iter()
                .find(|i| i.count > 0)
                .map(|i| format!("Top issue:  {} — +{}  ({:.2}/s)", i.label, i.count, i.rate))
                .unwrap_or_else(|| "Top issue:  none".to_string());

            // Clipboard miss breakdown (only when relevant)
            let clip_breakdown = if delta.clipboard_expansion_miss() > 0 {
                format!(
                    " (empty={}, type_mismatch={})",
                    delta.clipboard_expansion_miss_empty, delta.clipboard_expansion_miss_type,
                )
            } else {
                String::new()
            };

            format!(
                "Context metrics — delta since last reset ({:.1}s)\n\
                 Baseline: {} ({:.1}s ago)\n\
                 \n\
                 Health: {}\n\
                 Reason: {}{}{}{}\n\
                 \n\
                 Freshness\n\
                 hard_stale_hit:               {}\n\
                 soft_stale_hit:               {}\n\
                 stale_refresh_triggered:      {}\n\
                 \n\
                 {}\n\
                 \n\
                 Correctness guardrails\n\
                 terminal_incoherent_filtered: {}\n\
                 \n\
                 Intent binding\n\
                 clipboard_expansion_used:     {}\n\
                 clipboard_expansion_miss:     {}{}",
                elapsed_secs,
                baseline_time,
                elapsed_secs,
                health_icon,
                health_reason,
                refresh_coverage,
                action_line,
                tip_line,
                rate(delta.hard_stale_hit),
                rate(delta.soft_stale_hit),
                rate(delta.stale_refresh_triggered),
                top_issue,
                rate(delta.terminal_incoherent_filtered),
                rate(delta.clipboard_expansion_used),
                rate(delta.clipboard_expansion_miss()),
                clip_breakdown,
            )
        }
    }
}

fn format_context(ctx: &EnvironmentContext) -> String {
    let mut lines = vec![format!("Context gathered in {}ms", ctx.gather_ms)];
    lines.push(format!(
        "Sources: active_window={} terminal={} ide_workspace={}",
        ctx.active_window_source, ctx.terminal_source, ctx.ide_workspace_source
    ));
    lines.push(String::new());

    // Window
    match &ctx.active_window {
        Some(w) => {
            let id_suffix = w
                .window_id
                .as_deref()
                .map(|id| format!(" id={id}"))
                .unwrap_or_default();
            lines.push(format!(
                "Window: {} ({}) pid={} terminal={} ide={}{}",
                w.title, w.wm_class, w.pid, w.is_terminal, w.is_ide, id_suffix
            ));
        }
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

    // Git root vs CWD mismatch
    if let (Some(cwd), Some(g)) = (&ctx.cwd, &ctx.git)
        && cwd != &g.repo_root
    {
        lines.push(format!("Git root differs from CWD: {}", g.repo_root));
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
            if let Some(ref ws_root) = p.workspace_root {
                let ws_count = p.workspace_scripts.len();
                lines.push(format!("Workspace: root={ws_root} scripts={ws_count}"));
            }
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

    // Clipboard
    match &ctx.clipboard {
        Some(clip) => {
            use crate::context::clipboard_detect::ClipboardContentType;
            let desc = match clip {
                ClipboardContentType::Url(u) => format!("URL: {u}"),
                ClipboardContentType::FilePath(p) => format!("File: {p}"),
                ClipboardContentType::IpAddress(ip) => format!("IP: {ip}"),
                ClipboardContentType::Json => "JSON".into(),
                ClipboardContentType::GitHash(h) => format!("Git hash: {h}"),
                ClipboardContentType::Uuid(u) => format!("UUID: {u}"),
                ClipboardContentType::ErrorTrace(msg) => format!("Error/stack trace: {msg}"),
                ClipboardContentType::Plain => "Plain text".into(),
            };
            lines.push(format!("Clipboard: {desc}"));
        }
        None => lines.push("Clipboard: empty".to_string()),
    }

    // Network
    match &ctx.network {
        Some(net) => {
            let ssid = net
                .ssid
                .as_deref()
                .map(|s| format!("ssid={s}"))
                .unwrap_or_else(|| "no WiFi".into());
            let vpn = if net.vpn_active { " vpn=active" } else { "" };
            lines.push(format!("Network: {ssid}{vpn}"));
        }
        None => lines.push("Network: none".to_string()),
    }

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
    let terminal_cwd_info = match (
        cache_stats.terminal_cwd_age_ms,
        cache_stats.terminal_cwd_source.as_deref(),
    ) {
        (Some(age), Some(src)) => format!("{age}ms ago ({src})"),
        _ => "empty".to_string(),
    };
    lines.push(format!(
        "Cache: git={}, docker={}, project={}, network={}, terminal_cwd={}",
        fmt(cache_stats.git_age_ms, cache_stats.git_invalidation),
        fmt(cache_stats.docker_age_ms, cache_stats.docker_invalidation),
        fmt(cache_stats.project_age_ms, cache_stats.project_invalidation),
        fmt(cache_stats.network_age_ms, cache_stats.network_invalidation),
        terminal_cwd_info,
    ));

    // Focus ring
    let ring_entries = crate::context::window_stack::ring_debug_entries();
    if !ring_entries.is_empty() {
        lines.push(format!("Focus ring: ({} entries)", ring_entries.len()));
        for (wm_class, pid, cwd, age_secs) in ring_entries.iter().take(5) {
            let cwd_str = cwd.as_deref().unwrap_or("?");
            lines.push(format!(
                "  {wm_class}(pid={pid}) {cwd_str} — {age_secs}s ago"
            ));
        }
    }

    // Terminal routing metrics
    let metrics = crate::context::metrics::snapshot();
    if metrics.terminal_route_hit > 0
        || metrics.terminal_route_busy > 0
        || metrics.terminal_route_fail > 0
        || metrics.terminal_route_no_protocol > 0
    {
        lines.push(format!(
            "Terminal routing: hit={} busy={} fail={} no_protocol={}",
            metrics.terminal_route_hit,
            metrics.terminal_route_busy,
            metrics.terminal_route_fail,
            metrics.terminal_route_no_protocol,
        ));
    }

    // Context-derived suggestions with provenance. This debug view has no db
    // handle, so it shows the CONTEXT half of the zero state (clipboard action
    // + workspace memory) — pins and app recents are plain data, inspectable
    // in the UI itself.
    let suggestions: Vec<crate::action_registry::CompletionItem> =
        crate::context::suggestions::clipboard_action(ctx)
            .into_iter()
            .chain(crate::context::suggestions::workspace_commands(
                ctx, None, 5,
            ))
            .collect();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ctx_args_flatten_from_structured_json() {
        // A constrained model sends the typed object; it flattens to the exact
        // spellings the string-matcher in `execute` accepts.
        assert_eq!(ctx_args_to_flat("{}"), "");
        assert_eq!(ctx_args_to_flat(r#"{"topic":"metrics"}"#), "metrics");
        assert_eq!(
            ctx_args_to_flat(r#"{"topic":"metrics","mode":"--reset"}"#),
            "metrics --reset"
        );
        assert_eq!(
            ctx_args_to_flat(r#"{"topic":"metrics","mode":"--rate"}"#),
            "metrics --rate"
        );
        // A plain-string caller (human, legacy) passes straight through.
        assert_eq!(ctx_args_to_flat("metrics --rate"), "metrics --rate");
        // Malformed JSON falls back to the raw string.
        assert_eq!(ctx_args_to_flat("{not json"), "{not json");
    }

    /// Drift guard: every flat rendering the grammar can produce must land on
    /// the branch it names — the string matcher accepts each spelling.
    #[tokio::test]
    async fn structured_calls_hit_the_branches_they_name() {
        let h = ContextDebugHandler::new();
        let ctx = ExecContext::default();

        let run = |args: &'static str| {
            let h = ContextDebugHandler::new();
            let ctx = ctx.clone();
            async move { h.execute(&ctx, args).await.unwrap() }
        };

        // Lifetime counters.
        let r = run(r#"{"topic":"metrics"}"#).await;
        let body = match r.output {
            crate::action_registry::Output::Text { ref body, .. } => body.clone(),
            ref other => panic!("expected text, got {other:?}"),
        };
        assert!(body.contains("process lifetime totals"), "{body}");

        // Baseline reset.
        let r = run(r#"{"topic":"metrics","mode":"--reset"}"#).await;
        let body = match r.output {
            crate::action_registry::Output::Text { ref body, .. } => body.clone(),
            ref other => panic!("expected text, got {other:?}"),
        };
        assert!(body.contains("Baseline reset"), "{body}");

        // Rate report (baseline was just set above).
        let r = run(r#"{"topic":"metrics","mode":"--rate"}"#).await;
        let body = match r.output {
            crate::action_registry::Output::Text { ref body, .. } => body.clone(),
            ref other => panic!("expected text, got {other:?}"),
        };
        assert!(body.contains("delta since last reset"), "{body}");

        // Empty call → the context snapshot branch (no snapshot set in tests).
        let r = h.execute(&ctx, "{}").await.unwrap();
        assert!(r.success);
    }
}

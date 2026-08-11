//! Conservative tool-catalog filtering for the agent.
//!
//! The agent is handed a `ToolDef` per registered command (~46 of them). Sending
//! all of them every turn is a fixed token tax and gives a small/fast model more
//! ways to pick wrong. This trims the set to what a query plausibly needs —
//! WITHOUT ever starving the agent:
//!
//! - a CORE set (launch, run, search, compute, files, system, AI-answer) is
//!   ALWAYS present, so the agent can always do the common things regardless of
//!   how the query is phrased;
//! - tools whose keyword/description overlap the query's words are added on top;
//! - if the query is short/vague OR too many tools match, the FULL catalog is
//!   sent — over-including is cheap; under-including breaks the task.
//!
//! The bar for keeping a tool is deliberately low. A wrongly-dropped tool is a
//! silent capability loss (the worst failure); a wrongly-kept one costs a few
//! tokens. So this errs, every time, toward keeping.

use crate::providers::ToolDef;

/// Tools the agent must ALWAYS have — the verbs behind the most common asks,
/// plus the ones a plan tends to need mid-way (open a file it found, run a
/// follow-up command). Matched against `ToolDef::name` (the handler id).
const CORE_TOOLS: &[&str] = &[
    "open", "run", "web", "calc", "file", "system", "ask", "url", "browse", "yt",
];

/// If a query is at least this many chars, filtering is attempted; shorter
/// queries are too ambiguous to filter safely, so they get the full catalog.
const MIN_QUERY_CHARS: usize = 8;

/// If keyword/description matching selects at least this fraction of the
/// catalog, the query is broad enough that filtering saves little and risks
/// dropping something needed — send everything instead.
const FULL_CATALOG_ABOVE_FRACTION: f32 = 0.5;

/// Return the tools to send for `query`. Never returns fewer than the core set
/// (intersected with what's registered); returns the full `catalog` unchanged
/// whenever filtering would be unsafe or unhelpful.
pub fn select_tools(query: &str, catalog: Vec<ToolDef>) -> Vec<ToolDef> {
    let q = query.trim().to_lowercase();
    if q.chars().count() < MIN_QUERY_CHARS || catalog.len() <= CORE_TOOLS.len() {
        return catalog;
    }

    let query_words: Vec<&str> = q
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 3) // skip "is"/"a"/"to" noise
        .collect();
    if query_words.is_empty() {
        return catalog;
    }

    let mut relevant = 0usize;
    let selected: Vec<ToolDef> = catalog
        .iter()
        .filter(|t| {
            let is_core = CORE_TOOLS.contains(&t.name.as_str());
            let hay = format!("{} {}", t.name, t.description).to_lowercase();
            let matches = query_words.iter().any(|w| hay.contains(w));
            if matches && !is_core {
                relevant += 1;
            }
            is_core || matches
        })
        .cloned()
        .collect();

    // Broad query (matched a large share) → the filter isn't earning its risk.
    let non_core = catalog.len().saturating_sub(CORE_TOOLS.len()).max(1);
    if (relevant as f32) / (non_core as f32) >= FULL_CATALOG_ABOVE_FRACTION {
        return catalog;
    }

    // Guard: never send an empty (or core-only-because-nothing-matched) set that
    // is somehow smaller than core — shouldn't happen, but be safe.
    if selected.is_empty() {
        return catalog;
    }
    selected
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(name: &str, desc: &str) -> ToolDef {
        ToolDef {
            name: name.into(),
            description: desc.into(),
        }
    }

    fn full_catalog() -> Vec<ToolDef> {
        vec![
            tool("open", "Launch a desktop application"),
            tool("run", "Execute a shell command"),
            tool("web", "Search the web"),
            tool("calc", "Evaluate a math expression"),
            tool("file", "Open a file or directory"),
            tool(
                "system",
                "System controls: shutdown, reboot, volume, brightness",
            ),
            tool("ask", "Ask a question"),
            tool("url", "Open a URL"),
            tool("browse", "Browse a directory"),
            tool("yt", "Search YouTube"),
            tool("screenshot", "Take a screenshot of the screen or a window"),
            tool("timer", "Countdown timers and stopwatch"),
            tool("weather", "Current weather and forecast"),
            tool("ssh", "Connect to an SSH host"),
            tool("packages", "Search and install system packages"),
            tool("services", "List and control systemd services"),
        ]
    }

    fn names(tools: &[ToolDef]) -> Vec<&str> {
        tools.iter().map(|t| t.name.as_str()).collect()
    }

    #[test]
    fn a_specific_query_keeps_core_plus_the_matching_tool() {
        let out = select_tools("take a screenshot of my window", full_catalog());
        let n = names(&out);
        assert!(n.contains(&"screenshot"), "the matched tool is kept: {n:?}");
        // Core survives even though the query doesn't name them.
        assert!(n.contains(&"open") && n.contains(&"run") && n.contains(&"web"));
        // And it actually trimmed something (weather/ssh/timer aren't relevant).
        assert!(out.len() < full_catalog().len(), "filtered: {n:?}");
        assert!(!n.contains(&"weather"));
    }

    #[test]
    fn a_short_query_gets_the_full_catalog() {
        let full = full_catalog();
        let out = select_tools("hi", full.clone());
        assert_eq!(out.len(), full.len(), "short query is not filtered");
    }

    #[test]
    fn a_broad_query_gets_the_full_catalog() {
        // Words that hit many descriptions → don't risk trimming.
        let full = full_catalog();
        let out = select_tools("search open run system a directory the web", full.clone());
        assert_eq!(out.len(), full.len(), "broad query sends everything");
    }

    #[test]
    fn a_query_matching_nothing_still_has_core() {
        let out = select_tools("xyzzy plugh flooble wugga", full_catalog());
        let n = names(&out);
        // Nothing matched, so only core — but the agent can still act.
        assert!(n.contains(&"open") && n.contains(&"run") && n.contains(&"calc"));
        assert!(!n.contains(&"screenshot"));
    }

    #[test]
    fn a_small_catalog_is_never_filtered() {
        let small = vec![tool("open", "x"), tool("run", "y")];
        let out = select_tools("take a screenshot please now", small.clone());
        assert_eq!(out.len(), small.len());
    }

    #[test]
    fn ssh_query_keeps_ssh() {
        let out = select_tools("ssh into my production server", full_catalog());
        assert!(names(&out).contains(&"ssh"));
    }
}

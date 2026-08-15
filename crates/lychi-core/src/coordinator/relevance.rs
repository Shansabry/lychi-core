//! Per-query tool selection for the agent — sends only the tools a query plausibly
//! needs, not the whole ~48-tool catalog.
//!
//! WHY. A flat catalog of every tool is expensive AND error-prone. Each tool's
//! schema is ~50 tokens, so 48 tools is ~2,300 input tokens on EVERY model
//! round-trip — a "what is a dolphin" that needs no tools still pays it. And the
//! research is consistent across model tiers: more tools lowers selection accuracy
//! (position bias, tool-choice confusion), while filtering to the few relevant ones
//! both cuts tokens ~80% and RAISES accuracy. So we send a small, query-relevant
//! subset.
//!
//! FAIL-SAFE by construction — the bar to keep a tool is low and the agent is never
//! starved:
//! - a small CORE set (the verbs behind the most common asks) is ALWAYS present;
//! - tools whose name/description match the query's words are added on top;
//! - a short or unrankable query gets the FULL catalog (over-including is cheap;
//!   under-including breaks the task);
//! - and the executor dispatches ANY tool by name regardless of what was sent, so
//!   a wrongly-dropped tool is still runnable if the model asks for it.
//!
//! HOW. Rank each tool's `name + description` against the recent conversation with
//! the same `nucleo` fuzzy matcher the launcher already trusts for file/app search
//! — no embedding model, no new dependency, no cold-start cost. For ~48 short tool
//! descriptions, fuzzy lexical ranking floats the right handful reliably.

use nucleo_matcher::pattern::{Atom, AtomKind, CaseMatching, Normalization};
use nucleo_matcher::{Config, Matcher, Utf32Str};

use crate::providers::{ChatMessage, Role, ToolDef};

/// How many recent user/assistant turns feed the ranking. The set should track
/// where the task IS now, not only how it opened — a plan that moved from "check
/// disk" to "open the biggest folder" needs `open`/`file` by the later step.
const CONTEXT_LOOKBACK_MESSAGES: usize = 6;

/// Tools the agent must ALWAYS have — the verbs behind the most common asks, plus
/// the ones a plan tends to need mid-way (open a file it found, run a follow-up).
/// Matched against `ToolDef::name`. Keeps the agent capable no matter how the query
/// is phrased, so filtering can never strand a common action.
const CORE_TOOLS: &[&str] = &["run", "web", "calc", "url", "browse"];

/// A query shorter than this is too ambiguous to rank safely → send everything.
const MIN_QUERY_CHARS: usize = 8;

/// If the query matches at least this fraction of the (non-core) catalog, it is
/// broad enough that filtering saves little and risks dropping something needed —
/// send the full catalog instead.
const FULL_CATALOG_ABOVE_FRACTION: f32 = 0.5;

/// How many query-matched tools to keep beyond the core set. Generous vs. the
/// research's top-3 (that uses semantic embeddings; nucleo is lexical, so a wider
/// net compensates) while still cutting the catalog by ~80% on a focused query.
const MAX_MATCHED_TOOLS: usize = 8;

/// Select the tools to send for the current conversation. Never returns fewer than
/// the core set (intersected with what's registered); returns the full `catalog`
/// whenever filtering would be unsafe or unhelpful. See the module docs for the
/// fail-safe guarantees.
pub fn select_tools(messages: &[ChatMessage], catalog: &[ToolDef]) -> Vec<ToolDef> {
    if catalog.len() <= CORE_TOOLS.len() {
        return catalog.to_vec();
    }
    let context = recent_context(messages);
    let ctx = context.trim();
    if ctx.chars().count() < MIN_QUERY_CHARS {
        return catalog.to_vec();
    }

    // One fuzzy atom per meaningful query word. Scoring per word and summing lets a
    // tool that matches the salient word ("screenshot") float up even when the rest
    // of the phrase ("of my window") matches nothing.
    let atoms: Vec<Atom> = ctx
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 3)
        .map(|w| {
            Atom::new(
                w,
                CaseMatching::Ignore,
                Normalization::Smart,
                AtomKind::Fuzzy,
                false,
            )
        })
        .collect();
    if atoms.is_empty() {
        return catalog.to_vec();
    }

    let mut matcher = Matcher::new(Config::DEFAULT);
    let mut buf = Vec::new();
    let mut matched: Vec<(usize, u32)> = Vec::new();
    for (i, t) in catalog.iter().enumerate() {
        if CORE_TOOLS.contains(&t.name.as_str()) {
            continue; // core is always kept; don't let it crowd the matched slots
        }
        let hay = format!("{} {}", t.name, t.description);
        let total: u32 = atoms
            .iter()
            .filter_map(|a| {
                buf.clear();
                let haystack = Utf32Str::new(&hay, &mut buf);
                a.score(haystack, &mut matcher).map(u32::from)
            })
            .sum();
        if total > 0 {
            matched.push((i, total));
        }
    }

    // Broad query (matched a large share) → filtering isn't earning its risk.
    let non_core = catalog.len().saturating_sub(CORE_TOOLS.len()).max(1);
    if (matched.len() as f32) / (non_core as f32) >= FULL_CATALOG_ABOVE_FRACTION {
        return catalog.to_vec();
    }

    matched.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    matched.truncate(MAX_MATCHED_TOOLS);
    let keep_matched: std::collections::HashSet<usize> = matched.iter().map(|(i, _)| *i).collect();

    // Assemble in catalog order (stable): core tools + the matched ones.
    let selected: Vec<ToolDef> = catalog
        .iter()
        .filter(|t| CORE_TOOLS.contains(&t.name.as_str()))
        .chain(
            catalog
                .iter()
                .enumerate()
                .filter(|(i, _)| keep_matched.contains(i))
                .map(|(_, t)| t),
        )
        .cloned()
        .collect();

    if selected.is_empty() {
        return catalog.to_vec();
    }
    selected
}

/// Concatenate the last few user/assistant text turns into one ranking query.
fn recent_context(messages: &[ChatMessage]) -> String {
    messages
        .iter()
        .rev()
        .filter(|m| matches!(m.role, Role::User | Role::Assistant))
        .take(CONTEXT_LOOKBACK_MESSAGES)
        .map(|m| m.content_text())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(name: &str, desc: &str) -> ToolDef {
        ToolDef {
            name: name.into(),
            description: desc.into(),
            mutates: false,
            input_schema: None,
        }
    }

    fn catalog() -> Vec<ToolDef> {
        vec![
            tool("run", "Execute a shell command"),
            tool("web", "Search the web"),
            tool("calc", "Evaluate a math expression"),
            tool("url", "Open a URL"),
            tool("browse", "Browse a directory"),
            tool("screenshot", "Take a screenshot of the screen or a window"),
            tool("weather", "Current weather and forecast"),
            tool("ssh", "Connect to an SSH host"),
            tool("packages", "Search and install system packages"),
            tool("timer", "Countdown timers and stopwatch"),
            tool("define", "Look up a word definition"),
            tool("media", "Control media playback"),
        ]
    }

    fn names(tools: &[ToolDef]) -> Vec<String> {
        tools.iter().map(|t| t.name.clone()).collect()
    }

    #[test]
    fn a_specific_query_keeps_core_plus_the_matching_tool() {
        let msgs = vec![ChatMessage::user("take a screenshot of my window")];
        let out = select_tools(&msgs, &catalog());
        let n = names(&out);
        assert!(
            n.contains(&"screenshot".to_string()),
            "matched tool kept: {n:?}"
        );
        // Core survives even though the query doesn't name it.
        assert!(n.contains(&"run".to_string()) && n.contains(&"web".to_string()));
        // And it actually trimmed the catalog.
        assert!(out.len() < catalog().len(), "filtered: {n:?}");
        assert!(!n.contains(&"weather".to_string()));
    }

    #[test]
    fn a_short_query_gets_the_full_catalog() {
        let msgs = vec![ChatMessage::user("hi")];
        assert_eq!(select_tools(&msgs, &catalog()).len(), catalog().len());
    }

    #[test]
    fn a_query_matching_nothing_still_has_core() {
        let msgs = vec![ChatMessage::user("xyzzy plugh flooble wugga")];
        let n = names(&select_tools(&msgs, &catalog()));
        assert!(n.contains(&"run".to_string()) && n.contains(&"calc".to_string()));
        assert!(!n.contains(&"screenshot".to_string()));
    }

    #[test]
    fn a_small_catalog_is_never_filtered() {
        let small = vec![tool("run", "x"), tool("web", "y")];
        let msgs = vec![ChatMessage::user("take a screenshot please now")];
        assert_eq!(select_tools(&msgs, &small).len(), small.len());
    }

    #[test]
    fn tracks_a_later_turn_not_just_the_first() {
        let msgs = vec![
            ChatMessage::user("what is 2+2"),
            ChatMessage::assistant("4. Want me to take a screenshot of the result?"),
        ];
        let n = names(&select_tools(&msgs, &catalog()));
        assert!(
            n.contains(&"screenshot".to_string()),
            "later turn steers: {n:?}"
        );
    }
}

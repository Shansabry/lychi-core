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
/// is phrased, so filtering can never strand a common action. Carries both the
/// group-tool names and the legacy standalone names so filtering behaves during
/// the grammar migration (a name absent from the catalog is simply inert).
const CORE_TOOLS: &[&str] = &[
    "run",
    FIND_TOOL,
    "web_tools",
    "files",
    "quick_tools",
    "web",
    "calc",
    "url",
    "browse",
];

/// A catalog at or under this size — count AND serialized weight — is sent
/// WHOLE every turn: no ranking, no per-query variation, a byte-stable prefix
/// the provider can prompt-cache. Filtering a lean catalog would trade cache
/// hits for nothing.
const FULL_SEND_MAX_TOOLS: usize = 12;

/// The catalog weight cap for full-send. Providers have real per-request token
/// budgets (Groq's free tier rejects ~8k-token requests outright), and the
/// tool block ships with EVERY turn on top of history + system prompt — a
/// catalog past this weight must be rationed even when the tool COUNT is
/// small. ~16KB ≈ ~4k tokens.
const FULL_SEND_MAX_BYTES: usize = 16 * 1024;

/// A query shorter than this is too ambiguous to rank safely.
const MIN_QUERY_CHARS: usize = 8;

/// How many query-matched tools to keep beyond the core set. Generous vs. the
/// research's top-3 (that uses semantic embeddings; nucleo is lexical, so a wider
/// net compensates) while still cutting the catalog by ~80% on a focused query.
const MAX_MATCHED_TOOLS: usize = 8;

/// Approximate wire weight of a catalog: descriptions + serialized schemas.
fn approx_payload_bytes(catalog: &[ToolDef]) -> usize {
    catalog
        .iter()
        .map(|t| {
            t.description.len()
                + t.input_schema
                    .as_ref()
                    .map(|s| s.to_string().len())
                    .unwrap_or(24)
        })
        .sum()
}

/// The always-present core subset, in catalog order.
fn core_subset(catalog: &[ToolDef]) -> Vec<ToolDef> {
    catalog
        .iter()
        .filter(|t| CORE_TOOLS.contains(&t.name.as_str()))
        .cloned()
        .collect()
}

/// Select the tools to send for the current conversation. A LEAN catalog (few
/// tools, light payload) is returned whole — the stable, cacheable ideal. A
/// heavy catalog is rationed: the core set always, query-matched tools on top,
/// and NEVER the whole thing on a vague query (that is exactly the request a
/// token-budgeted provider rejects). The model recovers from any miss via
/// `find_tool` (core) and the executor's run-any-tool-by-name fail-safe.
pub fn select_tools(messages: &[ChatMessage], catalog: &[ToolDef]) -> Vec<ToolDef> {
    if catalog.len() <= FULL_SEND_MAX_TOOLS && approx_payload_bytes(catalog) <= FULL_SEND_MAX_BYTES
    {
        return catalog.to_vec();
    }
    let context = recent_context(messages);
    let ctx = context.trim();
    if ctx.chars().count() < MIN_QUERY_CHARS {
        return core_subset(catalog);
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
        return core_subset(catalog);
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
        return core_subset(catalog);
    }
    selected
}

/// Per-conversation sticky selection: [`select_tools`] proposes this step's
/// shortlist, `sent` (the session's append-only record) absorbs any new names,
/// and the returned set is every ever-sent tool in stable catalog order.
///
/// Append-only on purpose: a schema the model has seen must stay visible for
/// the rest of the conversation — history referencing a vanished tool confuses
/// models, and a prefix that only grows caches better than one that churns.
/// The trade: a vague turn that pulls in the full catalog keeps it for the
/// whole conversation. That is the safe direction to be wrong in.
pub fn select_tools_sticky(
    messages: &[ChatMessage],
    catalog: &[ToolDef],
    sent: &mut Vec<String>,
) -> Vec<ToolDef> {
    for t in select_tools(messages, catalog) {
        if !sent.iter().any(|n| n == &t.name) {
            sent.push(t.name);
        }
    }
    catalog
        .iter()
        .filter(|t| sent.iter().any(|n| n == &t.name))
        .cloned()
        .collect()
}

/// The coordinator's built-in discovery pseudo-tool. Not a registry handler:
/// the loop answers it inline (see the batch step in `loop_`), because its job
/// is to search the FULL catalog and widen the session's sent set — state only
/// the coordinator holds. This is the recovery path for a shortlist miss: the
/// model notices a capability gap, searches by task words, and the matching
/// schemas join the very next turn.
pub const FIND_TOOL: &str = "find_tool";

/// The `ToolDef` for [`FIND_TOOL`], sent with every shortlist (it is in
/// [`CORE_TOOLS`], so filtering can never drop the recovery path itself).
pub fn find_tool_def() -> ToolDef {
    ToolDef {
        name: FIND_TOOL.to_string(),
        description: "Search this launcher's full tool catalog when no visible tool fits the \
                      task. Returns matching tool names and descriptions; the matches become \
                      callable on your next step. Use task words (e.g. \"compress files\", \
                      \"wifi\"), not questions."
            .to_string(),
        mutates: false,
        mutating_actions: Vec::new(),
        input_schema: Some(serde_json::json!({
            "type": "object",
            "properties": {
                "query": { "type": "string",
                           "description": "Task words describing the needed capability." }
            },
            "required": ["query"],
            "additionalProperties": false
        })),
    }
}

/// How many catalog matches a `find_tool` search returns.
const FIND_TOOL_RESULTS: usize = 5;

/// Rank `query` against the catalog (name + description, same scoring as
/// [`select_tools`]) and return the best matches. Empty when nothing scores.
pub fn search_catalog<'a>(query: &str, catalog: &'a [ToolDef]) -> Vec<&'a ToolDef> {
    let atoms: Vec<Atom> = query
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
        return Vec::new();
    }
    let mut matcher = Matcher::new(Config::DEFAULT);
    let mut buf = Vec::new();
    let mut matched: Vec<(usize, u32)> = Vec::new();
    for (i, t) in catalog.iter().enumerate() {
        if t.name == FIND_TOOL {
            continue; // never offer the search tool as its own answer
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
    matched.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    matched.truncate(FIND_TOOL_RESULTS);
    matched.into_iter().map(|(i, _)| &catalog[i]).collect()
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
            mutating_actions: Vec::new(),
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
            // Padding beyond FULL_SEND_MAX_TOOLS so these tests exercise the
            // RANKING path — a catalog at or under the threshold is sent whole
            // and never filtered (covered by its own test below).
            tool("note", "Save and list notes"),
            tool("service", "Manage systemd services"),
            tool("emoji", "Search emoji"),
            tool("color", "Convert color formats"),
        ]
    }

    #[test]
    fn a_small_catalog_is_sent_whole() {
        let small: Vec<ToolDef> = catalog().into_iter().take(FULL_SEND_MAX_TOOLS).collect();
        let msgs = vec![ChatMessage::user("take a screenshot of my window please")];
        assert_eq!(select_tools(&msgs, &small).len(), small.len());
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
    fn a_short_query_on_a_heavy_catalog_gets_core_only() {
        // A vague query used to fall back to the FULL catalog — which is
        // exactly the request a token-budgeted provider rejects outright.
        // Core (with find_tool for recovery) is the safe floor now.
        let msgs = vec![ChatMessage::user("hi")];
        let out = select_tools(&msgs, &catalog());
        let n = names(&out);
        assert!(out.len() < catalog().len(), "not the full catalog: {n:?}");
        assert!(n.contains(&"run".to_string()) && n.contains(&"web".to_string()));
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
    fn sticky_selection_only_grows() {
        let mut sent = Vec::new();
        let msgs1 = vec![ChatMessage::user("take a screenshot of my window")];
        let first = select_tools_sticky(&msgs1, &catalog(), &mut sent);
        assert!(first.iter().any(|t| t.name == "screenshot"));
        let n_first = first.len();

        // A later turn about something else: the screenshot schema must survive
        // (history references it), and the new topic's tool joins.
        let msgs2 = vec![
            ChatMessage::user("take a screenshot of my window"),
            ChatMessage::user("now control the media playback please"),
        ];
        let second = select_tools_sticky(&msgs2, &catalog(), &mut sent);
        assert!(
            second.iter().any(|t| t.name == "screenshot"),
            "once sent, a tool stays: {:?}",
            names(&second)
        );
        assert!(second.iter().any(|t| t.name == "media"));
        assert!(second.len() >= n_first, "the sent set never shrinks");
    }

    #[test]
    fn search_catalog_ranks_the_named_tool_first() {
        let cat = catalog();
        let hits = search_catalog("take a screenshot", &cat);
        assert_eq!(hits.first().map(|t| t.name.as_str()), Some("screenshot"));
    }

    #[test]
    fn search_catalog_empty_query_matches_nothing() {
        assert!(search_catalog("", &catalog()).is_empty());
    }

    #[test]
    fn find_tool_never_returns_itself() {
        let mut cat = catalog();
        cat.push(find_tool_def());
        let hits = search_catalog("find a tool for this task", &cat);
        assert!(hits.iter().all(|t| t.name != FIND_TOOL));
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

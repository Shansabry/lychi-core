//! Per-query tool selection for the agent, sized to real provider budgets.
//!
//! WHY. The tool block ships with EVERY model round-trip on top of system
//! prompt + history, and providers have hard per-request token budgets (Groq's
//! free tier rejects ~8k-token requests outright). A LEAN catalog (few tools,
//! light payload) is sent whole — byte-stable, so the provider can prompt-cache
//! the prefix. A HEAVY catalog is rationed per query, and never falls back to
//! "send everything" — that is exactly the request budgeted providers reject.
//!
//! FAIL-SAFE by construction — the agent is never starved:
//! - a small CORE set (`run`, `web_tools`, `find_tool`) is ALWAYS present;
//! - tools whose name/description/action-names match the query's words join it;
//! - `find_tool` (core) lets the model search the full catalog when its
//!   shortlist misses, and the executor dispatches ANY tool by name regardless
//!   of what was sent, so a wrongly-dropped tool is still runnable.
//!
//! HOW. Deterministic word-level matching of the recent conversation against
//! each tool's name + description + action names: stopwords removed, then
//! exact word equality (strong) or 4+-char prefix overlap (morphology:
//! remind/reminder). Deliberately NOT the launcher's nucleo fuzzy matcher —
//! subsequence scoring could not separate a real short-word hit from scatter
//! noise across the rich group descriptions (measured: "note" 114 vs
//! "dolphin"-noise 102), and selection needs precision more than typo
//! tolerance. Scores SUM across query words so a chained request ("summarize
//! this and add it to notes") ranks every intent's group. Tuned by tests
//! against the real group tools, in both crates.

use crate::providers::{ChatMessage, Role, ToolDef};

/// How many recent user/assistant turns feed the ranking. The set should track
/// where the task IS now, not only how it opened — a plan that moved from "check
/// disk" to "open the biggest folder" needs `open`/`file` by the later step.
const CONTEXT_LOOKBACK_MESSAGES: usize = 6;

/// Tools the agent must ALWAYS have: shell, web, and the discovery tool that
/// recovers from any shortlist miss. Deliberately minimal — every core entry
/// is a token cost on EVERY turn, and the ranked groups (whose haystacks
/// include their action names) cover domain asks. Matched against
/// `ToolDef::name`; a name absent from the catalog is simply inert.
const CORE_TOOLS: &[&str] = &["run", FIND_TOOL, "web_tools"];

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

/// Question/function words that carry no tool intent. Without this filter,
/// nucleo's SUBSEQUENCE matching lets "what" or "please" scatter-match letters
/// across a 150-char group description and drag the whole catalog in — the
/// bug where "what is a dolphin" shipped every schema, twice.
const STOPWORDS: &[&str] = &[
    "the", "and", "for", "you", "your", "with", "from", "this", "that", "these", "those", "what",
    "when", "where", "which", "who", "whose", "why", "how", "can", "could", "would", "should",
    "will", "shall", "may", "might", "must", "please", "know", "tell", "let", "give", "get",
    "make", "need", "want", "like", "just", "some", "any", "all", "about", "into", "onto", "does",
    "did", "done", "have", "has", "had", "are", "was", "were", "been", "being", "not", "now",
    "then", "than", "there", "here", "its", "it's", "mine", "our", "out",
];

/// The floor a tool's score must clear to count as query-matched: one solid
/// word match. A prefix match scores exactly this; an exact word match scores
/// higher (see `score_tool`), and anything below is no lexical evidence at all.
const MIN_MATCH_SCORE: u32 = 120;

/// The meaningful words of a query: lowercased, alphanumeric-split, minus
/// stopwords and short fragments.
fn query_words(ctx: &str) -> Vec<String> {
    ctx.split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 3)
        .map(|w| w.to_lowercase())
        .filter(|w| !STOPWORDS.contains(&w.as_str()))
        .collect()
}

/// What a tool is ranked against: its name, description, and — for a grouped
/// tool — its action names. The action names are where the exact domain words
/// live (`calc`, `weather`, `note_add`), so "check the weather" ranks
/// `quick_tools` even though its prose says "instant answers".
fn tool_haystack(t: &ToolDef) -> String {
    let mut hay = format!("{} {}", t.name, t.description);
    if let Some(actions) = t
        .input_schema
        .as_ref()
        .and_then(|s| s["properties"]["action"]["enum"].as_array())
    {
        for a in actions {
            if let Some(name) = a.as_str() {
                hay.push(' ');
                // Compound names split into their words so "note" hits "note_add".
                hay.push_str(&name.replace('_', " "));
            }
        }
    }
    hay
}

/// Deterministic WORD-level scoring — not fuzzy. Nucleo's subsequence scoring
/// could not separate a genuine short-word hit from scatter noise here (a real
/// "note" match scored 114 while "dolphin" coincidences reached 102), so
/// selection matches whole words: exact equality is a strong hit, and a
/// one-sided prefix of at least 4 chars covers morphology (remind/reminder,
/// package/packages, window/windows). Per query word take the best hay-word
/// hit; SUM across query words so a chained request ("summarize this and add
/// it to notes") ranks every intent's group. Typos lose their tolerance — the
/// model recovers via `find_tool`, and agent queries are mostly clean text.
fn score_tool(words: &[String], t: &ToolDef) -> u32 {
    let hay = tool_haystack(t).to_lowercase();
    let hay_words: Vec<&str> = hay
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 3)
        .collect();
    words
        .iter()
        .map(|qw| {
            hay_words
                .iter()
                .map(|hw| {
                    if qw == hw {
                        200
                    } else if (qw.len() >= 4 && hw.starts_with(qw.as_str()))
                        || (hw.len() >= 4 && qw.starts_with(hw))
                    {
                        MIN_MATCH_SCORE
                    } else {
                        0
                    }
                })
                .max()
                .unwrap_or(0)
        })
        .sum()
}

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
pub fn select_tools(
    messages: &[ChatMessage],
    catalog: &[ToolDef],
    byte_budget: Option<usize>,
) -> Vec<ToolDef> {
    let within = |bytes: usize| byte_budget.is_none_or(|b| bytes <= b);
    if catalog.len() <= FULL_SEND_MAX_TOOLS
        && approx_payload_bytes(catalog) <= FULL_SEND_MAX_BYTES
        && within(approx_payload_bytes(catalog))
    {
        return catalog.to_vec();
    }
    let context = recent_context(messages);
    let ctx = context.trim();
    if ctx.chars().count() < MIN_QUERY_CHARS {
        return core_subset(catalog);
    }

    let words = query_words(ctx);
    if words.is_empty() {
        return core_subset(catalog);
    }

    let mut matched: Vec<(usize, u32)> = Vec::new();
    for (i, t) in catalog.iter().enumerate() {
        if CORE_TOOLS.contains(&t.name.as_str()) {
            continue; // core is always kept; don't let it crowd the matched slots
        }
        let total = score_tool(&words, t);
        if total >= MIN_MATCH_SCORE {
            matched.push((i, total));
        }
    }

    matched.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    matched.truncate(MAX_MATCHED_TOOLS);

    // Provider byte budget (the Tool Search Tool architecture's deferral):
    // core is always sent, then matched groups best-first WHILE they fit.
    // A shed group is DEFERRED, not gone — `find_tool` searches the full
    // catalog and widens the sent set, so the model can still summon it.
    let mut spent: usize = catalog
        .iter()
        .filter(|t| CORE_TOOLS.contains(&t.name.as_str()))
        .map(approx_tool_bytes)
        .sum();
    let mut keep_matched: std::collections::HashSet<usize> = Default::default();
    for (i, _) in &matched {
        let cost = approx_tool_bytes(&catalog[*i]);
        if within(spent + cost) {
            spent += cost;
            keep_matched.insert(*i);
        }
    }

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
    byte_budget: Option<usize>,
) -> Vec<ToolDef> {
    for t in select_tools(messages, catalog, byte_budget) {
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
    let words = query_words(query);
    if words.is_empty() {
        return Vec::new();
    }
    let mut matched: Vec<(usize, u32)> = Vec::new();
    for (i, t) in catalog.iter().enumerate() {
        if t.name == FIND_TOOL {
            continue; // never offer the search tool as its own answer
        }
        let total = score_tool(&words, t);
        if total >= MIN_MATCH_SCORE {
            matched.push((i, total));
        }
    }
    matched.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    matched.truncate(FIND_TOOL_RESULTS);
    matched.into_iter().map(|(i, _)| &catalog[i]).collect()
}

/// Concatenate the last few user/assistant text turns into one ranking query.
/// Approximate serialized size of one tool definition, for the byte budget.
fn approx_tool_bytes(t: &ToolDef) -> usize {
    serde_json::to_string(t).map(|j| j.len()).unwrap_or(0)
}

fn recent_context(messages: &[ChatMessage]) -> String {
    messages
        .iter()
        .rev()
        .filter(|m| matches!(m.role, Role::User | Role::Assistant))
        .take(CONTEXT_LOOKBACK_MESSAGES)
        .map(|m| strip_material_blocks(&m.content_text()))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Remove injected `<context>…</context>` and `<pasted>…</pasted>` blocks
/// before ranking. Both ride INSIDE the user message and both are material,
/// not intent: the ambient block's words ("Working directory", "Local time")
/// ranked `files`/`system_control` on every turn, and a pasted article's
/// content words did the same ("close your eyes… the monitor" summoned
/// `system_control` + `quick_tools`, 10.6KB of junk that blew Groq's TPM
/// estimate). Selection must judge the user's INSTRUCTION, nothing else.
pub(crate) fn strip_material_blocks(text: &str) -> String {
    let mut out = text.to_string();
    for (open, close) in [("<context>", "</context>"), ("<pasted>", "</pasted>")] {
        let mut acc = String::with_capacity(out.len());
        let mut rest = out.as_str();
        while let Some(start) = rest.find(open) {
            acc.push_str(&rest[..start]);
            match rest[start..].find(close) {
                Some(end) => rest = &rest[start + end + close.len()..],
                None => {
                    rest = ""; // unterminated block: drop the tail
                    break;
                }
            }
        }
        acc.push_str(rest);
        out = acc;
    }
    out
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
        assert_eq!(select_tools(&msgs, &small, None).len(), small.len());
    }

    fn names(tools: &[ToolDef]) -> Vec<String> {
        tools.iter().map(|t| t.name.clone()).collect()
    }

    #[test]
    fn a_specific_query_keeps_core_plus_the_matching_tool() {
        let msgs = vec![ChatMessage::user("take a screenshot of my window")];
        let out = select_tools(&msgs, &catalog(), None);
        let n = names(&out);
        assert!(
            n.contains(&"screenshot".to_string()),
            "matched tool kept: {n:?}"
        );
        // Core survives even though the query doesn't name it.
        assert!(n.contains(&"run".to_string()), "{n:?}");
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
        let out = select_tools(&msgs, &catalog(), None);
        let n = names(&out);
        assert!(out.len() < catalog().len(), "not the full catalog: {n:?}");
        assert!(n.contains(&"run".to_string()), "{n:?}");
    }

    #[test]
    fn a_query_matching_nothing_still_has_core() {
        let msgs = vec![ChatMessage::user("xyzzy plugh flooble wugga")];
        let n = names(&select_tools(&msgs, &catalog(), None));
        assert!(n.contains(&"run".to_string()), "{n:?}");
        assert!(!n.contains(&"screenshot".to_string()));
    }

    #[test]
    fn a_small_catalog_is_never_filtered() {
        let small = vec![tool("run", "x"), tool("web", "y")];
        let msgs = vec![ChatMessage::user("take a screenshot please now")];
        assert_eq!(select_tools(&msgs, &small, None).len(), small.len());
    }

    // ── Precision against the REAL group tools ───────────────────────────────
    // Built from the actual ToolGroup names/descriptions plus representative
    // action enums, so threshold tuning tracks the strings production ships.

    fn group_tool(g: crate::action_registry::grammar::ToolGroup, actions: &[&str]) -> ToolDef {
        ToolDef {
            name: g.tool_name().into(),
            description: g.description().into(),
            mutates: false,
            mutating_actions: Vec::new(),
            input_schema: Some(serde_json::json!({
                "type": "object",
                "properties": { "action": { "enum": actions } },
            })),
        }
    }

    fn production_like_catalog() -> Vec<ToolDef> {
        use crate::action_registry::grammar::ToolGroup as G;
        vec![
            group_tool(
                G::Files,
                &[
                    "browse", "file", "project", "zip", "extract", "convert", "resize",
                ],
            ),
            group_tool(G::Web, &["web", "url", "yt", "define", "bm", "quicklink"]),
            group_tool(
                G::System,
                &[
                    "system_volume",
                    "system_wifi",
                    "system_shutdown",
                    "screenshot_full",
                    "screenshot_area",
                    "win_focus",
                    "win_close",
                    "packages_install",
                    "service_restart",
                    "open",
                    "sysinfo",
                ],
            ),
            group_tool(G::Media, &["media"]),
            group_tool(
                G::Dev,
                &["devutil_base64", "devutil_hash", "ssh", "script", "ctx"],
            ),
            group_tool(
                G::Personal,
                &[
                    "note_add",
                    "note_read",
                    "todo_add",
                    "reminder_add",
                    "timer_start",
                    "snip",
                    "clip",
                    "alias_add",
                ],
            ),
            group_tool(
                G::Utils,
                &[
                    "calc",
                    "time",
                    "weather",
                    "emoji",
                    "color",
                    "qr",
                    "generate_password",
                    "sym",
                ],
            ),
            ToolDef {
                name: "run".into(),
                description: "Execute a shell command on this machine".into(),
                mutates: true,
                mutating_actions: Vec::new(),
                input_schema: None,
            },
            find_tool_def(),
        ]
    }

    /// Force the ranking path regardless of payload heuristics: pad the
    /// catalog past FULL_SEND_MAX_TOOLS with inert entries.
    fn heavy(mut cat: Vec<ToolDef>) -> Vec<ToolDef> {
        while cat.len() <= FULL_SEND_MAX_TOOLS {
            cat.push(tool(
                Box::leak(format!("pad{}", cat.len()).into_boxed_str()),
                "inert padding tool",
            ));
        }
        cat
    }

    #[test]
    fn precision_a_trivia_question_matches_no_groups() {
        // The 11K-token bug: "what is a dolphin" scatter-matched every rich
        // group description and shipped the whole catalog, twice. Stopwords +
        // the score floor must hold this to the core set.
        let cat = heavy(production_like_catalog());
        let msgs = vec![ChatMessage::user("can you let me know what is a dolphin?")];
        let n = names(&select_tools(&msgs, &cat, None));
        assert_eq!(
            n,
            vec![
                "web_tools".to_string(),
                "run".to_string(),
                FIND_TOOL.to_string()
            ],
            "trivia must ship core only"
        );
    }

    #[test]
    fn precision_domain_words_pull_their_groups() {
        let cat = heavy(production_like_catalog());
        let cases: &[(&str, &str)] = &[
            ("take a screenshot of my window", "system_control"),
            ("remind me tomorrow at 9am to buy milk", "personal_data"),
            ("check the weather in tokyo", "quick_tools"),
            ("zip up these log files", "files"),
            ("pause the music playback", "media_control"),
            // Chained intents: every action leg must rank its group.
            (
                "summarize this text and add it to my notes",
                "personal_data",
            ),
        ];
        for (query, expected) in cases {
            let msgs = vec![ChatMessage::user(*query)];
            let n = names(&select_tools(&msgs, &cat, None));
            assert!(
                n.contains(&expected.to_string()),
                "{query:?} should rank {expected}: got {n:?}"
            );
            assert!(
                n.len() <= 3 + CORE_TOOLS.len(),
                "{query:?} over-included: {n:?}"
            );
        }
    }

    #[test]
    fn precision_the_ambient_context_block_ranks_nothing() {
        // The ambient block rides INSIDE the user message and its own words
        // ("Working directory", "Project type", "Local time", "Package
        // manager") name several groups — a trivia question with the block
        // attached must still ship core only.
        let cat = heavy(production_like_catalog());
        let msgs = vec![ChatMessage::user(
            "what is a dolphin?\n\n<context>\n- Local time: 2026-08-17 Sunday 16:45\n\
             - Working directory: /mnt/DevSSD/Lychi\n- Git branch: main\n\
             - Project type: Rust\n- Package manager: pnpm\n\
             - Docker: 2 running container(s)\n</context>",
        )];
        let n = names(&select_tools(&msgs, &cat, None));
        assert_eq!(
            n,
            vec![
                "web_tools".to_string(),
                "run".to_string(),
                FIND_TOOL.to_string()
            ],
            "the ambient context block must not rank tools"
        );
    }

    #[test]
    fn precision_find_tool_search_stays_selective() {
        let cat = production_like_catalog();
        let hits = search_catalog("compress files into an archive", &cat);
        assert_eq!(hits.first().map(|t| t.name.as_str()), Some("files"));
        assert!(
            search_catalog("what is it", &cat).is_empty(),
            "stopword-only query must match nothing"
        );
    }

    #[test]
    fn sticky_selection_only_grows() {
        let mut sent = Vec::new();
        let msgs1 = vec![ChatMessage::user("take a screenshot of my window")];
        let first = select_tools_sticky(&msgs1, &catalog(), &mut sent, None);
        assert!(first.iter().any(|t| t.name == "screenshot"));
        let n_first = first.len();

        // A later turn about something else: the screenshot schema must survive
        // (history references it), and the new topic's tool joins.
        let msgs2 = vec![
            ChatMessage::user("take a screenshot of my window"),
            ChatMessage::user("now control the media playback please"),
        ];
        let second = select_tools_sticky(&msgs2, &catalog(), &mut sent, None);
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
        let n = names(&select_tools(&msgs, &catalog(), None));
        assert!(
            n.contains(&"screenshot".to_string()),
            "later turn steers: {n:?}"
        );
    }
}

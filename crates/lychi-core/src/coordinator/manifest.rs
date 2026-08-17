//! The agent capability manifest — a generated, token-lean overview of every
//! tool and AI command the agent can reach.
//!
//! WHY THIS EXISTS. The model is handed a JSON schema per tool, but a schema is
//! just `name` + one terse line + a `{args: string}` blob — no argument syntax,
//! no "when to use vs when not". So the model guessed args (`system` with no idea
//! it takes `shutdown`/`volume`) and confused overlapping one-liners (`web` vs
//! `ask` vs `run ls`) — the "random tool call" failure. The rich usage text
//! already existed, but in a table nothing sent to the model.
//!
//! This manifest folds that knowledge into compact prose: every tool with its
//! purpose + argument syntax, and every AI command (preset) so the agent knows
//! those exist too. It is generated from the live registry and presets store, so
//! it never drifts from what is actually registered.
//!
//! CURRENT WIRING: the full tool manifest ([`build_manifest`]) is NOT sent — tool
//! knowledge rides the callable schemas instead (each `ToolDef` description
//! carries the handler's `usage()`, and [`super::select_tools`] filters the set
//! per query). Only the presets note ([`build_presets_note`]) is spliced into the
//! agent's system prompt, since presets are not tools and the model can't learn
//! them any other way. The full-manifest builder stays for any surface that wants
//! the whole catalog as prose (e.g. a future stable-catalog design).
//!
//! Whatever is spliced becomes part of the system prompt, which should stay
//! byte-stable across turns — never fold per-turn state into it; per-turn context
//! belongs in a trailing message.

use crate::action_registry::CommandInfo;
use crate::ai_presets::AiPresetItem;

/// Build the capability-manifest block to append to the agent's system prompt.
///
/// `tools` is the FULL catalog (`registry.command_catalog()`), not the filtered
/// set — the agent should know every capability exists even when a given query
/// only ships a subset of callable schemas. `presets` is the AI-command list.
///
/// Returns an empty string when there is nothing to describe, so an empty
/// registry (tests, headless) adds no stray prose.
pub fn build_manifest(tools: &[CommandInfo], presets: &[AiPresetItem]) -> String {
    if tools.is_empty() && presets.is_empty() {
        return String::new();
    }

    let mut out = String::new();
    out.push_str(MANIFEST_MARKER);
    out.push_str(
        "\n### Your tools\n\
         Call a tool by its name with `args`. Pick the most specific tool for the \
         request; reach for `run` only for a genuine shell task no other tool covers. \
         If a tool errors or returns 'command not found', do NOT retry the same call — \
         pick a better-suited tool or ask the user; never repeat a failing call. \
         Each line is `name — what it does. args: how to call it`.\n",
    );

    for t in tools {
        // `name — description`, plus `. args: <usage>` only when the handler
        // declared usage. A mutating tool is flagged so the model weighs it.
        out.push_str("- `");
        out.push_str(&t.id);
        out.push_str("` — ");
        out.push_str(t.description.trim_end_matches('.'));
        if !t.usage.trim().is_empty() {
            out.push_str(". args: ");
            out.push_str(t.usage.trim());
        }
        if t.mutates {
            out.push_str(" [changes system state]");
        }
        out.push('\n');
    }

    out.push_str(&presets_section(presets));
    out
}

/// The AI-commands (presets) note ALONE — no tool list — as a spliceable block.
///
/// When tool knowledge is carried by the callable schemas (the token-lean default),
/// the agent still needs to know its saved AI presets exist so it can point the
/// user to one. This is small (one line per preset) and, unlike the tool list, does
/// not grow with the catalog — safe to always include. Returns "" when there are no
/// presets. Opens with [`MANIFEST_MARKER`] so [`splice_manifest`] stays idempotent
/// whichever block is spliced.
pub fn build_presets_note(presets: &[AiPresetItem]) -> String {
    let section = presets_section(presets);
    if section.is_empty() {
        return String::new();
    }
    format!("{MANIFEST_MARKER}\n{section}")
}

/// The presets list body (no marker) — shared by [`build_manifest`] and
/// [`build_presets_note`]. Empty when there are no presets.
fn presets_section(presets: &[AiPresetItem]) -> String {
    if presets.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "\n### AI commands\n\
         These are saved prompt templates the user can run by keyword on their text \
         or selection. You do not call them as tools, but know they exist so you can \
         point the user to one when it fits (e.g. \"try `summarize <text>`\").\n",
    );
    for p in presets {
        out.push_str("- `");
        out.push_str(&p.keyword);
        out.push_str("` — ");
        out.push_str(&p.name);
        out.push('\n');
    }
    out
}

/// The heading every generated capability block opens with — a stable splice
/// anchor. [`splice_manifest`] replaces everything from this marker down, so
/// re-augmenting the same session never stacks copies, whichever block is spliced
/// (full tool manifest or the presets-only note).
pub const MANIFEST_MARKER: &str = "## Capabilities";

/// Fold `manifest` onto `base`, replacing any manifest already appended.
///
/// A continued conversation re-runs prompt assembly each turn on a session whose
/// system prompt ALREADY carries last turn's manifest; naively appending would
/// stack a copy per turn. So this cuts `base` at [`MANIFEST_MARKER`] first, then
/// appends the fresh block — idempotent, and it also picks up a preset the user
/// added mid-conversation. An empty `manifest` just returns the trimmed base.
pub fn splice_manifest(base: &str, manifest: &str) -> String {
    let base = match base.find(MANIFEST_MARKER) {
        Some(i) => base[..i].trim_end(),
        None => base.trim_end(),
    };
    if manifest.is_empty() {
        return base.to_string();
    }
    format!("{base}\n\n{manifest}")
}

/// Fold the manifest into an existing system prompt: the base persona/rules first,
/// then the generated capability block. Returns `base` unchanged when the manifest
/// is empty. Keeping the base first means the agent's identity and tool-choice
/// rules lead; the enumerated catalog is reference material below them.
pub fn with_manifest(base: &str, tools: &[CommandInfo], presets: &[AiPresetItem]) -> String {
    splice_manifest(base, &build_manifest(tools, presets))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action_registry::{CommandCategory, CommandInfo};

    fn tool(id: &str, desc: &str, usage: &str, mutates: bool) -> CommandInfo {
        CommandInfo {
            id: id.to_string(),
            keyword: id.to_string(),
            description: desc.to_string(),
            usage: usage.to_string(),
            category: CommandCategory::General,
            category_title: "General".to_string(),
            category_order: 0,
            mutates,
            input_schema: None,
        }
    }

    fn preset(keyword: &str, name: &str) -> AiPresetItem {
        AiPresetItem {
            id: format!("id-{keyword}"),
            keyword: keyword.to_string(),
            name: name.to_string(),
            template: "{input}".to_string(),
            created_at: 0,
            updated_at: 0,
            is_builtin: true,
        }
    }

    #[test]
    fn empty_inputs_produce_no_prose() {
        assert_eq!(build_manifest(&[], &[]), "");
        assert_eq!(with_manifest("BASE", &[], &[]), "BASE");
    }

    #[test]
    fn a_tool_with_usage_emits_its_args() {
        let m = build_manifest(
            &[tool(
                "system",
                "System controls",
                "shutdown, volume <0-100>",
                true,
            )],
            &[],
        );
        assert!(m.contains("`system` — System controls"));
        assert!(m.contains("args: shutdown, volume <0-100>"));
        assert!(m.contains("[changes system state]"));
    }

    #[test]
    fn a_tool_without_usage_omits_the_args_clause() {
        let m = build_manifest(&[tool("calc", "Evaluate math", "", false)], &[]);
        // The tool's own line carries no args clause (the header prose may mention
        // "args:" generically, so assert on the calc line specifically).
        let calc_line = m.lines().find(|l| l.contains("`calc`")).unwrap();
        assert_eq!(calc_line, "- `calc` — Evaluate math");
        assert!(!calc_line.contains("args:"));
        assert!(!calc_line.contains("[changes system state]"));
    }

    #[test]
    fn presets_are_listed_under_their_own_heading() {
        let m = build_manifest(&[], &[preset("summarize", "Summarize")]);
        assert!(m.contains("## AI commands"));
        assert!(m.contains("`summarize` — Summarize"));
    }

    #[test]
    fn with_manifest_keeps_the_base_first() {
        let out = with_manifest("You are Lychi.", &[tool("calc", "Math", "", false)], &[]);
        assert!(out.starts_with("You are Lychi."));
        assert!(out.contains("## Your tools"));
    }

    #[test]
    fn splice_is_idempotent_across_turns() {
        // Re-augmenting a session that already carries the manifest must not stack
        // a second copy — the follow-up-turn hazard.
        let manifest = build_manifest(&[tool("calc", "Math", "", false)], &[]);
        let once = splice_manifest("You are Lychi.", &manifest);
        let twice = splice_manifest(&once, &manifest);
        assert_eq!(once, twice);
        assert_eq!(twice.matches(MANIFEST_MARKER).count(), 1);
        assert!(twice.starts_with("You are Lychi."));
    }

    #[test]
    fn splice_with_empty_manifest_strips_a_prior_one() {
        let manifest = build_manifest(&[tool("calc", "Math", "", false)], &[]);
        let with = splice_manifest("BASE", &manifest);
        // Tools turned off on a later turn → the stale manifest is removed.
        assert_eq!(splice_manifest(&with, ""), "BASE");
    }
}

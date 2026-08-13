/**
 * The single source of truth for "what does Enter do?" — the FRONTEND half.
 *
 * String CLASSIFICATION (is this a command, a question, a preset, a panel, a
 * typo?) lives entirely in the BACKEND now (`crates/lychi-core/src/intent/
 * classify.rs`), returned as a typed `RouteDecision`. The frontend no longer
 * re-derives it from a hardcoded keyword list — that list drifted out of sync
 * with the real handler set and was the root of routing inconsistencies.
 *
 * This module is the KEYBOARD/MODE/SELECTION reducer: it owns only the UI state
 * the backend can't see — keyboard modifiers (Ctrl/Shift), `/`-search & `@`-browse
 * modes, the selected completion row, and guards — and folds in the backend's
 * `RouteDecision` for everything string-classified. `decideSubmit` stays PURE
 * (no IPC, no Svelte state), so it's trivially testable; the backend decision is
 * passed in via `ctx.inputDecision` (and `ctx.runDecision` for a selected row's
 * `run`), fetched by the caller.
 *
 * Design rule (Sab): there is ONE AI path and ONE classifier. Natural language
 * routes to the agent/fork-card per the backend's `nl.confident`; deterministic
 * commands run verbatim. No keyword table is duplicated across the boundary.
 */

import type { RouteDecision } from "./bindings";

export type { RouteDecision };

/** A completion row as the router needs to see it (subset of CompletionItem). */
export interface RouterCompletion {
	label: string;
	/** The exact command to run when chosen, if the backend declared one. */
	run?: string | null;
	/** Tab-to-complete text (an argument-needing hint), if any. */
	fill?: string | null;
	/** Free-form description; for a "Did you mean" row this holds the correction. */
	description?: string | null;
	/** Sentinel icon path; drives a few special cases (separators, context). */
	icon_path?: string | null;
	/** Typed row kind, when selection behaves specially. Never match on `label`. */
	kind?: string | null;
}

/** Everything `decideSubmit` needs to know about the current UI state. */
export interface SubmitContext {
	/** The trimmed input text. */
	trimmed: string;
	/** Ctrl was held on Enter. */
	ctrlKey: boolean;
	/** Shift was held on Enter (capture `run` output inline). */
	runInline: boolean;
	/** `/`-search mode is active. */
	searchMode: boolean;
	/** `@`-browse mode is active. */
	atMode: boolean;
	/** The visible completions. */
	completions: RouterCompletion[];
	/** Index of the selected completion, or -1 if none. */
	completionIndex: number;
	/**
	 * The backend's classification of the current raw input — the single source
	 * of truth for command/nl/preset/panel/typo/ai-disabled. Fetched by the
	 * caller (attached to completions, or via `classifyInput`). When absent
	 * (a race before the first decision lands) the caller should await
	 * `classifyInput` before dispatching.
	 */
	inputDecision?: RouteDecision;
	/**
	 * The backend's classification of the SELECTED completion's `run` string,
	 * when it differs from the raw input (a history/context row echoing a prior
	 * command). Lets a replayed `run` route exactly as if typed fresh.
	 */
	runDecision?: RouteDecision;
	/**
	 * Files are staged in the attachment tray. Pure UI state (the backend can't
	 * see the tray), so it lives here: it makes an otherwise-empty submit
	 * meaningful — "here's an image, look at it" is a complete ask.
	 */
	hasAttachments?: boolean;
}

/** Render a preset template, substituting `{input}` (mirrors the Rust `render`). */
export function renderPreset(template: string, input: string): string {
	if (template.includes("{input}")) return template.replaceAll("{input}", input);
	if (!input) return template;
	return `${template}\n\n${input}`;
}

/**
 * A user message longer than this (chars) is folded into a collapsed attachment
 * chip in the chat bubble instead of shown inline. Keep in sync with the store's
 * intent; the store re-checks so callers can't accidentally inline a huge blob.
 */
export const PRESET_ATTACH_THRESHOLD = 240;

/**
 * Split a preset into what the CHAT BUBBLE shows: the instruction line (the
 * template minus the payload) and, when the payload is large, a collapsed
 * attachment chip. The MODEL still gets the full rendered prompt separately
 * (`renderPreset`) — this only shapes the on-screen bubble.
 *
 * - `Summarize the following: {input}` + big blob → instruction `Summarize the
 *   following:`, attachment `{label: "Selected text · 1.2k", body: blob}`.
 * - `translate {input} to spanish` + big blob → instruction `translate … to
 *   spanish`, attachment as above.
 * - Small input, or a template with no `{input}` → instruction is the rendered
 *   prompt, no attachment (behaves like before).
 */
export function presetDisplay(
	template: string,
	input: string,
): { instruction: string; attachment?: { label: string; body: string } } {
	const payload = input.trim();
	const large = payload.length >= PRESET_ATTACH_THRESHOLD;

	if (!large || !template.includes("{input}")) {
		// Nothing worth folding out — show the rendered prompt as-is.
		return { instruction: renderPreset(template, input) };
	}

	// Replace the `{input}` slot with an ellipsis so the instruction reads
	// naturally ("translate … to spanish", "Summarize the following: …").
	const instruction = template.replaceAll("{input}", "…").replace(/\s+/g, " ").trim();
	const label = `Selected text · ${formatCharCount(payload.length)}`;
	return { instruction, attachment: { label, body: payload } };
}

/** Compact char count for the attachment chip: 812, 1.2k, 34k. */
function formatCharCount(n: number): string {
	if (n < 1000) return `${n}`;
	if (n < 10_000) return `${(n / 1000).toFixed(1).replace(/\.0$/, "")}k`;
	return `${Math.round(n / 1000)}k`;
}

/**
 * The result of a submit decision — a tagged union. `handleSubmit` switches on
 * `kind` and does the (impure) work; everything routing-related is decided here.
 */
export type SubmitAction =
	/** Do nothing (empty input, a plan is showing, etc.). */
	| { kind: "noop" }
	/** Open a named panel. */
	| { kind: "panel"; panel: PanelName; notesTab?: NotesTab }
	/** Reveal the selected file-search result in the file manager (Ctrl+Enter). */
	| { kind: "reveal" }
	/** Drill into / open the selected @-browse or /-search completion. */
	| { kind: "completion-select"; label: string; ctrlKey: boolean }
	/** Fill the input with tab-to-complete text (an argument hint), then re-suggest. */
	| { kind: "fill"; value: string }
	/**
	 * Apply a correction. `run: false` (the default) FILLS the input so the user
	 * can confirm — right for an auto-offered guess on a typo they typed. `run:
	 * true` executes immediately: the user already picked the row, so asking
	 * them to press Enter a second time is pure friction.
	 */
	| { kind: "correct"; value: string; run?: boolean }
	/** Show a calc result inline without executing anything. */
	| { kind: "calc-display"; text: string }
	/** Try to open a literal filesystem path (search mode, no selectable result). */
	| { kind: "open-path"; path: string }
	/** Run a deterministic command through the executor. */
	| { kind: "command"; command: string; runInline?: boolean }
	/**
	 * An unknown query — show the fork card: a short streamed answer with
	 * [Search web] / [Full chat] buttons. This is the default for ambiguous
	 * natural language; the user then chooses whether to escalate or bail out.
	 */
	| { kind: "quick-ai"; prompt: string }
	/**
	 * Hand the query straight to the full tool-calling agent, skipping the fork
	 * card. A clear question (backend `nl.confident`) or an explicit `ask <q>`.
	 */
	| { kind: "agent"; prompt: string }
	/**
	 * An AI preset invocation. `template` + the user's `input`; the dispatcher
	 * renders it, and when `input` is empty it first tries the PRIMARY selection
	 * (highlighted text) as `{input}` — so `summarize` alone acts on the selection.
	 */
	| { kind: "preset"; template: string; input: string }
	/**
	 * The user made an AI request but no provider is configured. Warn them, then
	 * run `command` (a `web …` fallback the backend chose). `explicit` = they
	 * typed `ask …` / a preset (a deliberate AI ask) vs. a bare NL query.
	 */
	| { kind: "ai-disabled"; command: string; explicit: boolean };

export type PanelName = "settings" | "history" | "media" | "notes" | "chat-history";
export type NotesTab = "notes" | "todos" | "reminders" | "timers" | "snippets";

const SEPARATOR = "__separator__";

/**
 * Map a backend `RouteDecision` (string classification) to a `SubmitAction`.
 * This is the ONLY place command/nl/preset/panel/typo/ai-disabled outcomes are
 * turned into FE actions — all sourced from the backend, none re-derived here.
 */
function fromDecision(decision: RouteDecision): SubmitAction {
	switch (decision.kind) {
		case "command":
			return { kind: "command", command: decision.command };
		case "nl":
			return decision.confident
				? { kind: "agent", prompt: decision.prompt }
				: { kind: "quick-ai", prompt: decision.prompt };
		case "preset":
			return { kind: "preset", template: decision.template, input: decision.input };
		case "panel":
			// `PanelKind` is already the FE panel name (`settings`…`chat-history`);
			// `sub_tab` is the notes sub-tab when present.
			return {
				kind: "panel",
				panel: decision.name,
				...(decision.sub_tab ? { notesTab: decision.sub_tab as NotesTab } : {}),
			};
		case "correct":
			return { kind: "correct", value: decision.corrected };
		case "ai-disabled":
			return { kind: "ai-disabled", command: decision.command, explicit: decision.explicit };
	}
}

/**
 * Decide what a submit should do. PURE — no state, no IPC, no side effects.
 *
 * Precedence:
 *   1. guards (empty input with nothing selected)
 *   2. keyboard modifiers (Ctrl reveal / Ctrl web / Shift inline)
 *   3. a selected completion (calc/fill/@-search/drill, or its classified `run`)
 *   4. search-mode literal path
 *   5. the backend's classification of the raw input (`inputDecision`)
 */
export function decideSubmit(ctx: SubmitContext): SubmitAction {
	const { trimmed, completions, completionIndex } = ctx;
	const hasSelected = completions.length > 0 && completionIndex >= 0;

	// 1. Guards.
	if (!trimmed && !hasSelected) {
		// Attachments alone are a complete ask ("here's a screenshot — what is
		// this?"). Send them to the full agent with a neutral instruction; the
		// files themselves carry the question. Without a tray, still a no-op.
		return ctx.hasAttachments
			? { kind: "agent", prompt: "Look at the attached file(s) and describe what you see." }
			: { kind: "noop" };
	}

	// 2. Keyboard modifiers (pure UI state — no classification needed).
	if (ctx.ctrlKey && ctx.searchMode) {
		return hasSelected ? { kind: "reveal" } : { kind: "noop" };
	}
	if (ctx.ctrlKey && trimmed) {
		return { kind: "command", command: `web ${trimmed}` };
	}
	if (ctx.runInline && trimmed) {
		return { kind: "command", command: trimmed, runInline: true };
	}

	// 3. A selected completion. In search mode, auto-select the first real row.
	let idx = completionIndex;
	if (ctx.searchMode && completions.length > 0 && idx < 0) {
		const first = completions.findIndex((c) => c.icon_path !== SEPARATOR);
		idx = first >= 0 ? first : 0;
	}
	const selected = idx >= 0 ? completions[idx] : undefined;
	if (selected && selected.icon_path !== SEPARATOR) {
		return decideSelectedCompletion(ctx, selected);
	}

	// 4. Search mode with nothing selectable → treat input as a literal path.
	if (ctx.searchMode) {
		return { kind: "open-path", path: trimmed };
	}

	// 5. Classify the raw input via the backend decision (single source of truth).
	if (ctx.inputDecision) {
		const action = fromDecision(ctx.inputDecision);
		// With files staged, an ambiguous question is no longer ambiguous — the
		// user attached material to ask ABOUT. Skip the fork card and answer it
		// properly. (Attachments are UI state the classifier can't see, so this
		// promotion belongs on this side of the boundary, not in the backend.)
		if (ctx.hasAttachments && action.kind === "quick-ai") {
			return { kind: "agent", prompt: action.prompt };
		}
		return action;
	}
	// No decision yet (race) — the caller awaits `classifyInput` and re-dispatches.
	return { kind: "noop" };
}

/**
 * Sub-decision for a selected completion row. Pure UI actuation (calc preview,
 * tab-complete, @/search drill) is decided here; the row's `run` string is
 * classified by the BACKEND (`ctx.runDecision`) so a replayed history/context
 * command routes exactly as if typed fresh.
 */
function decideSelectedCompletion(ctx: SubmitContext, selected: RouterCompletion): SubmitAction {
	const { trimmed } = ctx;

	// @-browse or /-search → drill into / open the row (these modes own Enter).
	if (ctx.atMode || ctx.searchMode) {
		return { kind: "completion-select", label: selected.label, ctrlKey: ctx.ctrlKey };
	}

	// A correction row is an EXPLICIT choice and always wins.
	//
	// Checked before the NL guard below. Since natural phrasing became
	// suggestible ("can you define gallop" → "define gallop"), the input itself
	// classifies as `nl` while the row carries no `run` — exactly the shape that
	// guard treats as "the question owns Enter". That made selecting the
	// suggestion silently open AI chat instead of running the command the user
	// just picked. The guard exists for AUTO-selected fallback rows; a row the
	// user deliberately highlighted is not one of those.
	if (selected.kind === "correction" && selected.description) {
		// Selected deliberately → run it. (An auto-offered typo correction still
		// only fills, so a wrong guess never executes on its own.)
		return { kind: "correct", value: selected.description, run: true };
	}

	// "Ask AI" — the user explicitly chose the agent over the suggested command,
	// so go straight there. Also checked before the NL guard: that guard exists
	// for AUTO-selected fallback rows and would otherwise substitute the input's
	// own (possibly ambiguous, or AI-disabled → web) decision for the explicit
	// choice just made.
	if (selected.kind === "ask-ai" && selected.description) {
		return { kind: "agent", prompt: selected.description };
	}
	if (selected.kind === "search-web" && selected.description) {
		return { kind: "command", command: `web ${selected.description}` };
	}
	// Both the raw input AND the highlighted row's `run` classify as natural
	// language → the question owns Enter. This is the case where an auto-selected
	// "Ask AI: …" / "Search web: …" fallback row (its `run` is `ask …`/`web …`)
	// echoes the NL query; letting it drive routing was the dual-path bug (a
	// question web-searching or dead-ending instead of reaching the agent).
	// A row whose `run` is a REAL command still wins below — a deliberately
	// highlighted command is not overridden.
	// Narrow to the decision itself rather than a boolean, so the non-null case is
	// carried by the type system instead of asserted.
	const nlInput =
		ctx.inputDecision?.kind === "nl" || ctx.inputDecision?.kind === "ai-disabled"
			? ctx.inputDecision
			: undefined;
	const runIsNl =
		!selected.run || ctx.runDecision?.kind === "nl" || ctx.runDecision?.kind === "ai-disabled";
	if (nlInput && runIsNl) {
		return fromDecision(nlInput);
	}

	// A calc row is an ANSWER — display it, never execute it. Identified by its
	// typed kind; the value is still read off the label because that IS the
	// rendered answer ("= 42" → "42").
	if (selected.kind === "calc") {
		return { kind: "calc-display", text: selected.label.replace(/^=\s*/, "") };
	}
	// Tab-complete hint → fill the input (an argument-needing command).
	//
	// Only when the row has NOTHING to run. A row may carry both: a multi-repo
	// pick offers `fill` so Tab can refine the query, and `run` so Enter
	// executes in the chosen repo. Checking `fill` first made Enter re-fill the
	// input instead of running — the row looked selected and did nothing.
	// Tab reads `fill` on its own path (`+page.svelte`), so deferring to `run`
	// here costs that behaviour nothing.
	if (selected.fill && !selected.run) {
		return { kind: "fill", value: selected.fill };
	}
	// The backend declared the exact command to run — classify it (the backend
	// graded `selected.run` into `runDecision`). A history/context row whose
	// `run` echoes a prior natural-language query routes to the agent; a real
	// command runs verbatim. This is the fix for the dual-path replay bug.
	if (selected.run) {
		if (ctx.runDecision) return fromDecision(ctx.runDecision);
		// No classification available — run verbatim (safe default; the backend
		// executor re-resolves the string anyway).
		return { kind: "command", command: selected.run };
	}

	// No explicit run/fill — infer the command from the input + label. The
	// backend's classification of the raw input tells us whether the input is a
	// known-prefix command (`inputDecision.kind === "command"`); the label is
	// the argument/target to complete.
	const inputIsCommand = ctx.inputDecision?.kind === "command";
	const spaceIdx = trimmed.indexOf(" ");
	if (spaceIdx !== -1) {
		const prefix = trimmed.slice(0, spaceIdx);
		if (inputIsCommand) {
			// "run htop" with label "run htop" would double the prefix — run label as-is.
			if (selected.label.toLowerCase().startsWith(`${prefix.toLowerCase()} `)) {
				return { kind: "command", command: selected.label };
			}
			return { kind: "command", command: `${prefix} ${selected.label}` };
		}
		// Multi-word non-command with a completion selected → classify the raw
		// input (agent/quick-ai per the backend), never a raw command replay.
		if (ctx.inputDecision) return fromDecision(ctx.inputDecision);
		return { kind: "noop" };
	}
	if (inputIsCommand && trimmed.toLowerCase() === selected.label.toLowerCase()) {
		// Bare single-token command that equals the label ("mem" → sysinfo "mem").
		return { kind: "command", command: trimmed };
	}
	if (inputIsCommand) {
		// Bare prefix ("clip", "focus") — prefix + selected label.
		return { kind: "command", command: `${trimmed} ${selected.label}` };
	}
	if (selected.icon_path === "__context__") {
		// Context suggestion — label is a complete command ("git commit").
		return { kind: "command", command: selected.label };
	}
	// No prefix — an app completion; launch via open.
	return { kind: "command", command: `open ${selected.label}` };
}

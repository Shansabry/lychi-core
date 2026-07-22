/**
 * The single source of truth for "what does Enter do?".
 *
 * `handleSubmit` in `+page.svelte` used to be a ~180-line ladder of inline
 * `if (…) { await runCommand(…); return; }` branches. Deciding whether a given
 * input went to the AI agent, a deterministic command, a panel, or a file-open
 * meant reading the whole ladder — and the branches were easy to get subtly
 * wrong (a natural-language query slipping into the old router, etc.).
 *
 * This module pulls that decision out into ONE pure function, `decideSubmit`,
 * that maps the current input + completion state to exactly one `SubmitAction`.
 * No side effects, no Svelte state, no IPC — so it's trivially unit-testable and
 * there is a single place to reason about routing. `handleSubmit` becomes a thin
 * `switch` that performs the chosen action.
 *
 * Design rule (Sab): there is ONE AI path. Any natural-language fallthrough
 * returns `{ kind: "agent" }` — never the old command router. Deterministic
 * commands still short-circuit to `{ kind: "command" }` and stay instant.
 */

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
	/** A destructive-action plan is showing (Enter is captured by it). */
	pendingPlan: boolean;
	/** The visible completions. */
	completions: RouterCompletion[];
	/** Index of the selected completion, or -1 if none. */
	completionIndex: number;
	/**
	 * Whether an AI provider is configured. When false there is no agent to
	 * route to, so natural-language queries fall straight to a web search
	 * instead of the (impossible) fork card. Sab's rule: no AI → rely on
	 * keywords/web, never a dead-end.
	 */
	aiEnabled: boolean;
	/**
	 * User-defined AI presets (keyword → prompt template). Typing
	 * `<keyword> <text>` renders the template and sends it to the full agent.
	 */
	presets?: AiPreset[];
}

/** An AI preset as the router needs it (subset of AiPresetItem). */
export interface AiPreset {
	keyword: string;
	/** The template with a `{input}` placeholder. */
	template: string;
}

/** Render a preset template, substituting `{input}` (mirrors the Rust `render`). */
export function renderPreset(template: string, input: string): string {
	if (template.includes("{input}")) return template.replaceAll("{input}", input);
	if (!input) return template;
	return `${template}\n\n${input}`;
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
	/** Fill the input with corrected text (typo "Did you mean"), then re-suggest. */
	| { kind: "correct"; value: string }
	/** Show a calc result inline without executing anything. */
	| { kind: "calc-display"; text: string }
	/** Try to open a literal filesystem path (search mode, no selectable result). */
	| { kind: "open-path"; path: string }
	/** Run a deterministic command through the executor. */
	| { kind: "command"; command: string; runInline?: boolean }
	/**
	 * An unknown query — show the fork card: a short streamed answer with
	 * [Search web] / [Full chat] buttons. This is the default for natural
	 * language; the user then chooses whether to escalate to the full agent
	 * or bail out to a web search. Never a dead-end.
	 */
	| { kind: "quick-ai"; prompt: string }
	/**
	 * Hand the query straight to the full tool-calling agent, skipping the fork
	 * card. Used for an EXPLICIT `ask <q>` — the user already said "AI".
	 */
	| { kind: "agent"; prompt: string }
	/**
	 * An AI preset invocation. `template` + the user's `input`; the dispatcher
	 * renders it, and when `input` is empty it first tries the PRIMARY selection
	 * (highlighted text) as `{input}` — so `summarize` alone acts on the
	 * selection. Kept separate from `agent` so that selection lookup (async IO)
	 * stays out of the pure router.
	 */
	| { kind: "preset"; template: string; input: string }
	/**
	 * The user made an AI request but no provider is configured. Warn them, then
	 * fall back to a web search for `prompt`. `explicit` = they typed `ask …`
	 * (a deliberate AI ask) vs. a bare natural-language query.
	 */
	| { kind: "ai-disabled"; prompt: string; explicit: boolean };

export type PanelName = "settings" | "history" | "media" | "notes" | "chat-history";
export type NotesTab = "notes" | "todos" | "reminders" | "timers" | "snippets";

/**
 * First words that ARE deterministic handler prefixes. A query whose first token
 * is one of these + more text is a command (`open spotify`, `run ls`); anything
 * else with a space is natural language → the agent.
 */
export const KNOWN_PREFIXES: ReadonlySet<string> = new Set([
	"ask",
	"bm",
	"bookmark",
	"browse",
	"clip",
	"clipboard",
	"clear",
	"close",
	"emoji",
	"focus",
	"kill",
	"open",
	"sym",
	"unicode",
	"web",
	"yt",
	"run",
	"calc",
	"file",
	"url",
	"media",
	"project",
	"quit",
	"system",
	"note",
	"notes",
	"todo",
	"todos",
	"weather",
	"sysinfo",
	"ip",
	"cpu",
	"mem",
	"disk",
	"temp",
	"gpu",
	"battery",
	"net",
	"audio",
	"display",
	"os",
	"speedtest",
	"time",
	"tz",
	"clock",
	"alias",
	"aliases",
	"timer",
	"stopwatch",
]);

/** Bare panel keywords: typing just this word opens a panel. */
const PANEL_KEYWORDS: Record<string, { panel: PanelName; notesTab?: NotesTab }> = {
	settings: { panel: "settings" },
	history: { panel: "history" },
	chat: { panel: "chat-history" },
	chats: { panel: "chat-history" },
	conversations: { panel: "chat-history" },
	spotify: { panel: "media" },
	media: { panel: "media" },
	music: { panel: "media" },
	note: { panel: "notes", notesTab: "notes" },
	notes: { panel: "notes", notesTab: "notes" },
	todo: { panel: "notes", notesTab: "todos" },
	todos: { panel: "notes", notesTab: "todos" },
	reminder: { panel: "notes", notesTab: "reminders" },
	reminders: { panel: "notes", notesTab: "reminders" },
	timer: { panel: "notes", notesTab: "timers" },
	timers: { panel: "notes", notesTab: "timers" },
	stopwatch: { panel: "notes", notesTab: "timers" },
	snip: { panel: "notes", notesTab: "snippets" },
	snippet: { panel: "notes", notesTab: "snippets" },
	snippets: { panel: "notes", notesTab: "snippets" },
};

const SEPARATOR = "__separator__";
const CONTEXT_ICON = "__context__";

/**
 * Is `cmd` a deterministic command (safe to run through the executor), or is it
 * natural language that should go to the agent?
 *
 * Deterministic when: a colon trigger ("tz:tokyo"), an absolute path ("/x"), a
 * single token (a bare app name or keyword — "firefox", "settings"), OR a
 * multi-word string whose first word is a known handler prefix ("open spotify",
 * "run ls"). Everything else — multi-word starting with a non-prefix word
 * ("what is rust?") — is natural language.
 *
 * This is what lets a `__history__` completion (whose `run` echoes whatever you
 * typed before) route correctly: a replayed "open spotify" runs; a replayed
 * "what is rust?" goes to the agent.
 */
export function isDeterministicCommand(cmd: string): boolean {
	const t = cmd.trim();
	if (!t) return false;
	if (t.startsWith("/") || t.startsWith("~/")) return true; // a path
	const lower = t.toLowerCase();
	if (/^[a-z]{1,4}:/.test(lower) && !lower.startsWith("http")) return true; // colon trigger
	const spaceIdx = t.indexOf(" ");
	if (spaceIdx === -1) return true; // single token — a bare app/keyword, run it
	const first = lower.slice(0, spaceIdx);
	return KNOWN_PREFIXES.has(first);
}

/**
 * Decide what a submit should do. PURE — no state, no IPC, no side effects.
 *
 * The ordering matters and mirrors the old ladder's precedence:
 *   1. guards (empty / plan showing)
 *   2. keyboard modifiers (Ctrl reveal / Ctrl web / Shift inline)
 *   3. bare panel keywords
 *   4. colon triggers
 *   5. a selected completion (with all its sub-cases)
 *   6. search-mode literal path
 *   7. fallthrough → the AI agent
 */
export function decideSubmit(ctx: SubmitContext): SubmitAction {
	const action = decideSubmitInner(ctx);
	// No AI configured → there's no agent or fork card to open. Surface an
	// `ai-disabled` action so the UI can warn the user, then fall to web.
	// `explicit` (they typed `ask …`/a preset) → agent-like; a bare NL query →
	// quick-ai. A preset has no meaningful web fallback text, so use its input.
	if (!ctx.aiEnabled) {
		if (action.kind === "quick-ai" || action.kind === "agent") {
			return { kind: "ai-disabled", prompt: action.prompt, explicit: action.kind === "agent" };
		}
		if (action.kind === "preset") {
			return { kind: "ai-disabled", prompt: action.input, explicit: true };
		}
	}
	return action;
}

function decideSubmitInner(ctx: SubmitContext): SubmitAction {
	const { trimmed, completions, completionIndex } = ctx;
	const lower = trimmed.toLowerCase();
	const hasSelected = completions.length > 0 && completionIndex >= 0;

	// 1. Guards.
	if (!trimmed && !hasSelected) return { kind: "noop" };
	if (ctx.pendingPlan) return { kind: "noop" };

	// 2. Keyboard modifiers.
	if (ctx.ctrlKey && ctx.searchMode) {
		return hasSelected ? { kind: "reveal" } : { kind: "noop" };
	}
	if (ctx.ctrlKey && trimmed) {
		return { kind: "command", command: `web ${trimmed}` };
	}
	if (ctx.runInline && trimmed) {
		return { kind: "command", command: trimmed, runInline: true };
	}

	// 2b. Explicit `ask <q>` → the full agent chat, skipping the fork card.
	// The user already said "AI", so don't make them pick again. Bare "ask"
	// (no query) falls through to the panel/normal handling.
	if (lower === "ask" || lower.startsWith("ask ")) {
		const q = trimmed.slice(3).trim();
		if (q) return { kind: "agent", prompt: q };
	}

	// 2c. AI preset invocation: `<keyword> <text>` (or a bare `<keyword>`).
	// The user typed a saved AI-command keyword → render its template with the
	// rest as {input} and send to the full agent. Explicit AI, so no fork card.
	if (ctx.presets && ctx.presets.length > 0) {
		const spaceIdx = trimmed.indexOf(" ");
		const firstWord = (spaceIdx === -1 ? lower : lower.slice(0, spaceIdx)).trim();
		const preset = ctx.presets.find((p) => p.keyword.toLowerCase() === firstWord);
		if (preset) {
			const rest = spaceIdx === -1 ? "" : trimmed.slice(spaceIdx + 1).trim();
			return { kind: "preset", template: preset.template, input: rest };
		}
	}

	// 3. Bare panel keywords.
	const panel = PANEL_KEYWORDS[lower];
	if (panel) return { kind: "panel", ...panel };

	// 4. Colon triggers (e.g. "al:list", "tz:tokyo") — straight to the backend.
	if (/^[a-z]{1,4}:/.test(lower) && !lower.startsWith("http")) {
		return { kind: "command", command: trimmed };
	}

	// In search mode, an unselected list auto-selects its first real row.
	let idx = completionIndex;
	if (ctx.searchMode && completions.length > 0 && idx < 0) {
		const first = completions.findIndex((c) => c.icon_path !== SEPARATOR);
		idx = first >= 0 ? first : 0;
	}

	// 5. A selected completion.
	const selected = idx >= 0 ? completions[idx] : undefined;
	if (selected && selected.icon_path !== SEPARATOR) {
		return decideSelectedCompletion(ctx, selected, lower);
	}

	// 6. Search mode with nothing selectable → treat input as a literal path.
	if (ctx.searchMode) {
		return { kind: "open-path", path: trimmed };
	}

	// 7. Fallthrough: an unknown query → the fork card (short answer + buttons).
	return { kind: "quick-ai", prompt: trimmed };
}

/** Sub-decision for when a completion row is selected. */
function decideSelectedCompletion(
	ctx: SubmitContext,
	selected: RouterCompletion,
	lower: string,
): SubmitAction {
	const { trimmed } = ctx;

	// Calc results ("= 42") — display, don't execute.
	if (selected.label.startsWith("= ")) {
		return { kind: "calc-display", text: selected.label.slice(2) };
	}
	// Typo suggestion ("Did you mean: X?") — fill with the correction (in `description`).
	if (selected.label.startsWith("Did you mean:") && selected.description) {
		return { kind: "correct", value: selected.description };
	}
	// @-browse or /-search → drill into / open the row.
	if (ctx.atMode || ctx.searchMode) {
		return { kind: "completion-select", label: selected.label, ctrlKey: ctx.ctrlKey };
	}
	// The backend declared the exact command to run — run it verbatim, UNLESS
	// that command is itself natural language. A `__history__` completion carries
	// `run = <the raw thing you typed before>`; if you once typed "what is rust?"
	// it comes back as a history row whose `run` is "what is rust?". Replaying
	// that as a command sends natural language to the executor (→ web fallback or,
	// before removal, the old `ask` router) — NOT the agent. So a `run` that
	// isn't a deterministic command routes to the agent, same as typing it fresh.
	if (selected.fill) {
		return { kind: "fill", value: selected.fill };
	}
	if (selected.run) {
		return isDeterministicCommand(selected.run)
			? { kind: "command", command: selected.run }
			: { kind: "quick-ai", prompt: selected.run };
	}

	// No explicit run/fill — infer from the input shape.
	const spaceIdx = trimmed.indexOf(" ");
	if (spaceIdx !== -1) {
		const prefix = trimmed.slice(0, spaceIdx).toLowerCase();
		if (KNOWN_PREFIXES.has(prefix)) {
			// "run htop" with label "run htop" would double the prefix — run label as-is.
			if (selected.label.toLowerCase().startsWith(`${prefix} `)) {
				return { kind: "command", command: selected.label };
			}
			return { kind: "command", command: `${prefix} ${selected.label}` };
		}
		// Natural language with a completion auto-selected → the fork card, NOT
		// the old router. This was the dual-path bug: a history/context completion
		// echoing the typed query routed raw text back through `route_command`.
		return { kind: "quick-ai", prompt: trimmed };
	}
	if (KNOWN_PREFIXES.has(lower)) {
		// Bare prefix ("clip", "focus") — prefix + selected label.
		return { kind: "command", command: `${lower} ${selected.label}` };
	}
	if (selected.icon_path === CONTEXT_ICON) {
		// Context suggestion — label is a complete command ("git commit").
		return { kind: "command", command: selected.label };
	}
	if (selected.label.toLowerCase() === lower) {
		// Completion equals the input ("mem" → sysinfo "mem"). A single-token
		// exact match is a command shortcut; multi-word is caught above.
		return { kind: "command", command: trimmed };
	}
	// No prefix — an app completion; launch via open.
	return { kind: "command", command: `open ${selected.label}` };
}

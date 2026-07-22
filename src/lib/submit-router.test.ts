import { describe, expect, it } from "vitest";
import {
	decideSubmit,
	isDeterministicCommand,
	type RouterCompletion,
	renderPreset,
	type SubmitContext,
} from "./submit-router";

/** A default context: idle box, no completions, no modifiers. */
function ctx(overrides: Partial<SubmitContext> = {}): SubmitContext {
	return {
		trimmed: "",
		ctrlKey: false,
		runInline: false,
		searchMode: false,
		atMode: false,
		pendingPlan: false,
		completions: [],
		completionIndex: -1,
		aiEnabled: true,
		...overrides,
	};
}

/** A completion row helper. */
function comp(over: Partial<RouterCompletion> = {}): RouterCompletion {
	return { label: "x", ...over };
}

describe("decideSubmit — guards", () => {
	it("empty input with no selection is a noop", () => {
		expect(decideSubmit(ctx()).kind).toBe("noop");
	});

	it("a showing plan swallows Enter", () => {
		expect(decideSubmit(ctx({ trimmed: "hi", pendingPlan: true })).kind).toBe("noop");
	});
});

describe("decideSubmit — keyboard modifiers", () => {
	it("Ctrl+Enter forces a web search", () => {
		expect(decideSubmit(ctx({ trimmed: "rust", ctrlKey: true }))).toEqual({
			kind: "command",
			command: "web rust",
		});
	});

	it("Ctrl+Enter in search mode reveals the selection", () => {
		const c = ctx({
			trimmed: "/home",
			ctrlKey: true,
			searchMode: true,
			completions: [comp()],
			completionIndex: 0,
		});
		expect(decideSubmit(c).kind).toBe("reveal");
	});

	it("Shift+Enter captures run output inline", () => {
		expect(decideSubmit(ctx({ trimmed: "run ls", runInline: true }))).toEqual({
			kind: "command",
			command: "run ls",
			runInline: true,
		});
	});
});

describe("decideSubmit — panels & colon triggers", () => {
	it("bare 'settings' opens the settings panel", () => {
		expect(decideSubmit(ctx({ trimmed: "settings" }))).toEqual({
			kind: "panel",
			panel: "settings",
		});
	});

	it("bare 'todos' opens notes on the todos tab", () => {
		expect(decideSubmit(ctx({ trimmed: "todos" }))).toEqual({
			kind: "panel",
			panel: "notes",
			notesTab: "todos",
		});
	});

	it("a colon trigger goes straight to the backend", () => {
		expect(decideSubmit(ctx({ trimmed: "tz:tokyo" }))).toEqual({
			kind: "command",
			command: "tz:tokyo",
		});
	});

	it("an http url is NOT treated as a colon trigger", () => {
		// falls through to the agent (no completions) rather than colon-routing
		expect(decideSubmit(ctx({ trimmed: "http://x" })).kind).toBe("quick-ai");
	});
});

describe("decideSubmit — the single AI path (the dual-path bug)", () => {
	it("a bare question with NO completions goes to the agent", () => {
		expect(decideSubmit(ctx({ trimmed: "what is rust?" }))).toEqual({
			kind: "quick-ai",
			prompt: "what is rust?",
		});
	});

	it("a bare question WITH an auto-selected history completion still goes to the agent", () => {
		// This is the exact regression: a history/context row echoing the typed
		// query used to route raw text back through the old command router.
		const c = ctx({
			trimmed: "what is rust?",
			completions: [comp({ label: "what is rust?", icon_path: "__context__" })],
			completionIndex: 0,
		});
		expect(decideSubmit(c)).toEqual({ kind: "quick-ai", prompt: "what is rust?" });
	});

	it("a __history__ completion whose `run` echoes the typed question goes to the agent (THE bug)", () => {
		// The real leak: history rows carry `run = <the raw thing you typed>`.
		// A prior "what is rust?" comes back with run="what is rust?", which used
		// to be replayed verbatim through the executor. It must go to the agent.
		const c = ctx({
			trimmed: "what is rust?",
			completions: [
				comp({ label: "what is rust?", icon_path: "__history__", run: "what is rust?" }),
			],
			completionIndex: 0,
		});
		expect(decideSubmit(c)).toEqual({ kind: "quick-ai", prompt: "what is rust?" });
	});

	it("a __history__ completion whose `run` IS a real command still runs", () => {
		const c = ctx({
			trimmed: "open spot",
			completions: [comp({ label: "open spotify", icon_path: "__history__", run: "open spotify" })],
			completionIndex: 0,
		});
		expect(decideSubmit(c)).toEqual({ kind: "command", command: "open spotify" });
	});

	it("a multi-word query whose selected label equals it goes to the agent, not a command", () => {
		const c = ctx({
			trimmed: "what is rust?",
			completions: [comp({ label: "what is rust?" })],
			completionIndex: 0,
		});
		expect(decideSubmit(c).kind).toBe("quick-ai");
	});

	it("natural language never yields a command/noop dead-end", () => {
		for (const q of ["explain closures", "summarize this", "why is the sky blue"]) {
			expect(decideSubmit(ctx({ trimmed: q })).kind).toBe("quick-ai");
		}
	});

	it("explicit `ask <q>` skips the fork card → full agent chat", () => {
		expect(decideSubmit(ctx({ trimmed: "ask what is rust?" }))).toEqual({
			kind: "agent",
			prompt: "what is rust?",
		});
	});

	it("bare `ask` with no query does not become an agent call", () => {
		// no query after "ask" → falls through (quick-ai on the bare word)
		expect(decideSubmit(ctx({ trimmed: "ask" })).kind).not.toBe("agent");
	});
});

describe("renderPreset", () => {
	it("substitutes {input}", () => {
		expect(renderPreset("Translate: {input}", "hola")).toBe("Translate: hola");
	});
	it("replaces every {input}", () => {
		expect(renderPreset("{input} / {input}", "x")).toBe("x / x");
	});
	it("appends when no placeholder and input present", () => {
		expect(renderPreset("Summarize:", "text")).toBe("Summarize:\n\ntext");
	});
	it("returns template as-is when no placeholder and no input", () => {
		expect(renderPreset("Say hi", "")).toBe("Say hi");
	});
});

describe("decideSubmit — AI presets", () => {
	const presets = [
		{ keyword: "translate", template: "Translate to English: {input}" },
		{ keyword: "email", template: "Write a professional email about: {input}" },
	];

	it("`translate hola` renders the template → full agent", () => {
		expect(decideSubmit(ctx({ trimmed: "translate hola", presets }))).toEqual({
			kind: "agent",
			prompt: "Translate to English: hola",
		});
	});

	it("a custom user preset works the same way", () => {
		expect(decideSubmit(ctx({ trimmed: "email quarterly results", presets }))).toEqual({
			kind: "agent",
			prompt: "Write a professional email about: quarterly results",
		});
	});

	it("a bare preset keyword with no input still renders (empty {input})", () => {
		expect(decideSubmit(ctx({ trimmed: "translate", presets }))).toEqual({
			kind: "agent",
			prompt: "Translate to English: ",
		});
	});

	it("a non-preset query is unaffected", () => {
		expect(decideSubmit(ctx({ trimmed: "what is rust?", presets })).kind).toBe("quick-ai");
	});

	it("preset matching is case-insensitive on the keyword", () => {
		expect(decideSubmit(ctx({ trimmed: "TRANSLATE hola", presets })).kind).toBe("agent");
	});
});

describe("decideSubmit — AI disabled warns then falls to web", () => {
	it("an unknown query → ai-disabled (implicit), carrying the prompt", () => {
		expect(decideSubmit(ctx({ trimmed: "what is rust?", aiEnabled: false }))).toEqual({
			kind: "ai-disabled",
			prompt: "what is rust?",
			explicit: false,
		});
	});

	it("explicit `ask <q>` → ai-disabled (explicit) when AI is off", () => {
		expect(decideSubmit(ctx({ trimmed: "ask what is rust?", aiEnabled: false }))).toEqual({
			kind: "ai-disabled",
			prompt: "what is rust?",
			explicit: true,
		});
	});

	it("deterministic commands are unaffected by AI being off", () => {
		expect(decideSubmit(ctx({ trimmed: "settings", aiEnabled: false }))).toEqual({
			kind: "panel",
			panel: "settings",
		});
	});
});

describe("isDeterministicCommand", () => {
	it("known-prefix multi-word is a command", () => {
		expect(isDeterministicCommand("open spotify")).toBe(true);
		expect(isDeterministicCommand("run ls -la")).toBe(true);
		expect(isDeterministicCommand("web cats")).toBe(true);
	});
	it("single token is a command (bare app/keyword)", () => {
		expect(isDeterministicCommand("firefox")).toBe(true);
		expect(isDeterministicCommand("settings")).toBe(true);
	});
	it("colon triggers and paths are commands", () => {
		expect(isDeterministicCommand("tz:tokyo")).toBe(true);
		expect(isDeterministicCommand("/home/sab")).toBe(true);
	});
	it("multi-word starting with a non-prefix word is natural language", () => {
		expect(isDeterministicCommand("what is rust?")).toBe(false);
		expect(isDeterministicCommand("explain closures to me")).toBe(false);
		expect(isDeterministicCommand("why is the sky blue")).toBe(false);
	});
});

describe("decideSubmit — deterministic commands stay instant", () => {
	it("a known-prefix query with a selected app completion runs the command", () => {
		const c = ctx({
			trimmed: "open spot",
			completions: [comp({ label: "Spotify" })],
			completionIndex: 0,
		});
		expect(decideSubmit(c)).toEqual({ kind: "command", command: "open Spotify" });
	});

	it("does not double a prefix already on the label", () => {
		const c = ctx({
			trimmed: "run htop",
			completions: [comp({ label: "run htop" })],
			completionIndex: 0,
		});
		expect(decideSubmit(c)).toEqual({ kind: "command", command: "run htop" });
	});

	it("a completion carrying an explicit `run` runs it verbatim", () => {
		const c = ctx({
			trimmed: "wea",
			completions: [comp({ label: "Search web: wea", run: "web wea" })],
			completionIndex: 0,
		});
		expect(decideSubmit(c)).toEqual({ kind: "command", command: "web wea" });
	});

	it("a completion carrying `fill` fills the box (tab-to-complete)", () => {
		const c = ctx({
			trimmed: "vol",
			completions: [comp({ label: "volume <n>", fill: "system volume " })],
			completionIndex: 0,
		});
		expect(decideSubmit(c)).toEqual({ kind: "fill", value: "system volume " });
	});
});

describe("decideSubmit — completions & search mode", () => {
	it("a calc result displays without executing", () => {
		const c = ctx({
			trimmed: "2+2",
			completions: [comp({ label: "= 4" })],
			completionIndex: 0,
		});
		expect(decideSubmit(c)).toEqual({ kind: "calc-display", text: "4" });
	});

	it("a 'Did you mean' row fills the correction from its description", () => {
		const c = ctx({
			trimmed: "opne firefox",
			completions: [comp({ label: "Did you mean: open firefox?", description: "open firefox" })],
			completionIndex: 0,
		});
		expect(decideSubmit(c)).toEqual({ kind: "correct", value: "open firefox" });
	});

	it("search mode drills into the selected row", () => {
		const c = ctx({
			trimmed: "/home",
			searchMode: true,
			completions: [comp({ label: "Documents" })],
			completionIndex: 0,
		});
		expect(decideSubmit(c)).toEqual({
			kind: "completion-select",
			label: "Documents",
			ctrlKey: false,
		});
	});

	it("search mode with no selectable row tries to open the literal path", () => {
		const c = ctx({ trimmed: "/home/sab/x", searchMode: true });
		expect(decideSubmit(c)).toEqual({ kind: "open-path", path: "/home/sab/x" });
	});

	it("search mode auto-selects the first non-separator row", () => {
		const c = ctx({
			trimmed: "/h",
			searchMode: true,
			completionIndex: -1,
			completions: [comp({ label: "sep", icon_path: "__separator__" }), comp({ label: "home" })],
		});
		expect(decideSubmit(c)).toEqual({
			kind: "completion-select",
			label: "home",
			ctrlKey: false,
		});
	});
});

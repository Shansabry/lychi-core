import { describe, expect, it } from "vitest";
import type { RouteDecision } from "./bindings";
import {
	decideSubmit,
	type RouterCompletion,
	renderPreset,
	type SubmitContext,
} from "./submit-router";

/**
 * These tests cover the FRONTEND reducer only: how keyboard/mode/selection state
 * composes with the BACKEND's `RouteDecision`. String classification itself
 * (command-vs-agent, colon triggers, preset/panel keywords, NL confidence, typo
 * correction) is the backend's job and is tested in Rust
 * (`crates/lychi-core/src/intent/classify.rs`). Here we feed a mock decision in
 * via `inputDecision`/`runDecision` and assert the reducer actuates it correctly.
 */

/** A default context: idle box, no completions, no modifiers, no decision. */
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
		...overrides,
	};
}

/** A completion row helper. */
function comp(over: Partial<RouterCompletion> = {}): RouterCompletion {
	return { label: "x", ...over };
}

const CMD = (command: string): RouteDecision => ({ kind: "command", command });
const NL = (prompt: string, confident: boolean): RouteDecision => ({
	kind: "nl",
	prompt,
	confident,
});

describe("decideSubmit — guards", () => {
	it("empty input with no selection is a noop", () => {
		expect(decideSubmit(ctx()).kind).toBe("noop");
	});

	it("a showing plan swallows Enter", () => {
		expect(decideSubmit(ctx({ trimmed: "hi", pendingPlan: true })).kind).toBe("noop");
	});

	it("input with no decision yet is a noop (caller awaits classifyInput)", () => {
		expect(decideSubmit(ctx({ trimmed: "firefox" })).kind).toBe("noop");
	});
});

describe("decideSubmit — staged attachments (FE-only UI state)", () => {
	it("attachments alone make an empty submit a full-agent turn", () => {
		const a = decideSubmit(ctx({ hasAttachments: true }));
		expect(a.kind).toBe("agent");
		if (a.kind === "agent") expect(a.prompt.length).toBeGreaterThan(0);
	});

	it("an empty submit with no attachments is still a noop", () => {
		expect(decideSubmit(ctx({ hasAttachments: false })).kind).toBe("noop");
	});

	it("attachments promote an ambiguous question past the fork card", () => {
		const base = { trimmed: "what is this", inputDecision: NL("what is this", false) };
		expect(decideSubmit(ctx(base)).kind).toBe("quick-ai");
		expect(decideSubmit(ctx({ ...base, hasAttachments: true })).kind).toBe("agent");
	});

	it("attachments do not hijack a deterministic command", () => {
		const a = decideSubmit(
			ctx({ trimmed: "firefox", inputDecision: CMD("open firefox"), hasAttachments: true }),
		);
		expect(a.kind).toBe("command");
	});

	it("a showing plan still swallows Enter even with attachments", () => {
		expect(decideSubmit(ctx({ pendingPlan: true, hasAttachments: true })).kind).toBe("noop");
	});
});

describe("decideSubmit — keyboard modifiers (FE-only, no classification)", () => {
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

describe("decideSubmit — actuating the backend decision", () => {
	it("a command decision runs verbatim", () => {
		expect(
			decideSubmit(ctx({ trimmed: "open spotify", inputDecision: CMD("open spotify") })),
		).toEqual({
			kind: "command",
			command: "open spotify",
		});
	});

	it("a confident NL decision goes straight to the full agent", () => {
		expect(
			decideSubmit(ctx({ trimmed: "what is rust?", inputDecision: NL("what is rust?", true) })),
		).toEqual({ kind: "agent", prompt: "what is rust?" });
	});

	it("an ambiguous NL decision shows the fork card (quick-ai)", () => {
		expect(
			decideSubmit(ctx({ trimmed: "pasta recipe", inputDecision: NL("pasta recipe", false) })),
		).toEqual({ kind: "quick-ai", prompt: "pasta recipe" });
	});

	it("a preset decision renders through", () => {
		const d: RouteDecision = { kind: "preset", template: "Translate: {input}", input: "hola" };
		expect(decideSubmit(ctx({ trimmed: "translate hola", inputDecision: d }))).toEqual({
			kind: "preset",
			template: "Translate: {input}",
			input: "hola",
		});
	});

	it("a panel decision maps to the FE panel + notes sub-tab", () => {
		const d: RouteDecision = { kind: "panel", name: "notes", sub_tab: "todos" };
		expect(decideSubmit(ctx({ trimmed: "todos", inputDecision: d }))).toEqual({
			kind: "panel",
			panel: "notes",
			notesTab: "todos",
		});
	});

	it("a panel decision with no sub-tab omits notesTab", () => {
		const d: RouteDecision = { kind: "panel", name: "settings", sub_tab: null };
		expect(decideSubmit(ctx({ trimmed: "settings", inputDecision: d }))).toEqual({
			kind: "panel",
			panel: "settings",
		});
	});

	it("a correct decision fills the correction for confirm", () => {
		const d: RouteDecision = { kind: "correct", corrected: "open Spotify" };
		expect(decideSubmit(ctx({ trimmed: "spoti", inputDecision: d }))).toEqual({
			kind: "correct",
			value: "open Spotify",
		});
	});

	it("an ai-disabled decision carries the web-fallback command", () => {
		const d: RouteDecision = { kind: "ai-disabled", command: "web what is rust?", explicit: true };
		expect(decideSubmit(ctx({ trimmed: "what is rust?", inputDecision: d }))).toEqual({
			kind: "ai-disabled",
			command: "web what is rust?",
			explicit: true,
		});
	});
});

describe("decideSubmit — selected completion actuation", () => {
	it("a calc row displays, does not execute", () => {
		const c = ctx({
			trimmed: "= 6*7",
			completions: [comp({ label: "= 42" })],
			completionIndex: 0,
			inputDecision: CMD("= 6*7"),
		});
		expect(decideSubmit(c)).toEqual({ kind: "calc-display", text: "42" });
	});

	it("a 'Did you mean' row fills the correction", () => {
		const c = ctx({
			trimmed: "spoti",
			completions: [comp({ label: "Did you mean: open Spotify?", description: "open Spotify" })],
			completionIndex: 0,
		});
		expect(decideSubmit(c)).toEqual({ kind: "correct", value: "open Spotify" });
	});

	it("a tab-complete hint fills the input", () => {
		const c = ctx({
			trimmed: "tz",
			completions: [comp({ label: "tz tokyo", fill: "tz " })],
			completionIndex: 0,
		});
		expect(decideSubmit(c)).toEqual({ kind: "fill", value: "tz " });
	});

	it("@-browse selection drills into the row", () => {
		const c = ctx({
			trimmed: "@src",
			atMode: true,
			completions: [comp({ label: "~/src/" })],
			completionIndex: 0,
		});
		expect(decideSubmit(c)).toEqual({ kind: "completion-select", label: "~/src/", ctrlKey: false });
	});
});

describe("decideSubmit — a selected row's run is classified by the backend (dual-path fix)", () => {
	it("a history row whose run is a real command runs verbatim", () => {
		const c = ctx({
			trimmed: "open spot",
			completions: [comp({ label: "open spotify", icon_path: "__history__", run: "open spotify" })],
			completionIndex: 0,
			inputDecision: NL("open spot", false),
			runDecision: CMD("open spotify"),
		});
		expect(decideSubmit(c)).toEqual({ kind: "command", command: "open spotify" });
	});

	it("a history row whose run echoes a past question goes to the agent", () => {
		const c = ctx({
			trimmed: "what is rust?",
			completions: [
				comp({ label: "what is rust?", icon_path: "__history__", run: "what is rust?" }),
			],
			completionIndex: 0,
			inputDecision: NL("what is rust?", true),
			runDecision: NL("what is rust?", true),
		});
		expect(decideSubmit(c)).toEqual({ kind: "agent", prompt: "what is rust?" });
	});

	it("an NL question whose selected row echoes the query (run is NL) goes to the agent", () => {
		// A history/context row echoing an NL query — both input and run classify
		// as NL, so the question owns Enter (not a raw replay). This is the
		// no-response fix: the input's classification wins over the row.
		const c = ctx({
			trimmed: "what is rust?",
			completions: [
				comp({ label: "what is rust?", icon_path: "__history__", run: "what is rust?" }),
			],
			completionIndex: 0,
			inputDecision: NL("what is rust?", true),
			runDecision: NL("what is rust?", true),
		});
		expect(decideSubmit(c)).toEqual({ kind: "agent", prompt: "what is rust?" });
	});

	it("an ambiguous NL query (no runnable row selected) shows the fork card", () => {
		// Inline fallback rows were removed backend-side, so a bare NL query has no
		// competing row — the input decision drives Enter.
		const c = ctx({
			trimmed: "pasta recipe",
			inputDecision: NL("pasta recipe", false),
		});
		expect(decideSubmit(c)).toEqual({ kind: "quick-ai", prompt: "pasta recipe" });
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

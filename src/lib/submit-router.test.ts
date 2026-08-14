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
// `_confident` is accepted-and-ignored: the backend `Nl` decision no longer
// carries a confidence flag (every NL query goes to the agent), but call sites
// still pass the old second arg. Kept optional so they need no churn.
const NL = (prompt: string, _confident?: boolean): RouteDecision => ({
	kind: "nl",
	prompt,
});

describe("decideSubmit — guards", () => {
	it("empty input with no selection is a noop", () => {
		expect(decideSubmit(ctx()).kind).toBe("noop");
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

	it("a natural-language query routes to the agent, with or without attachments", () => {
		const base = { trimmed: "what is this", inputDecision: NL("what is this") };
		expect(decideSubmit(ctx(base)).kind).toBe("agent");
		expect(decideSubmit(ctx({ ...base, hasAttachments: true })).kind).toBe("agent");
	});

	it("attachments do not hijack a deterministic command", () => {
		const a = decideSubmit(
			ctx({ trimmed: "firefox", inputDecision: CMD("open firefox"), hasAttachments: true }),
		);
		expect(a.kind).toBe("command");
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

	it("every NL decision — question or ambiguous — goes to the full agent", () => {
		expect(
			decideSubmit(ctx({ trimmed: "what is rust?", inputDecision: NL("what is rust?") })),
		).toEqual({ kind: "agent", prompt: "what is rust?" });
		expect(
			decideSubmit(ctx({ trimmed: "pasta recipe", inputDecision: NL("pasta recipe") })),
		).toEqual({ kind: "agent", prompt: "pasta recipe" });
	});

	it("a preset decision renders through", () => {
		const d: RouteDecision = {
			kind: "preset",
			keyword: "translate",
			template: "Translate: {input}",
			input: "hola",
		};
		expect(decideSubmit(ctx({ trimmed: "translate hola", inputDecision: d }))).toEqual({
			kind: "preset",
			keyword: "translate",
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
			completions: [comp({ label: "= 42", kind: "calc" })],
			completionIndex: 0,
			inputDecision: CMD("= 6*7"),
		});
		expect(decideSubmit(c)).toEqual({ kind: "calc-display", text: "42" });
	});

	it("a correction row fills the correction", () => {
		const c = ctx({
			trimmed: "spoti",
			completions: [
				comp({
					label: "Did you mean: open Spotify?",
					description: "open Spotify",
					kind: "correction",
				}),
			],
			completionIndex: 0,
		});
		expect(decideSubmit(c)).toEqual({ kind: "correct", value: "open Spotify", run: true });
	});

	it("a selected 'Ask AI' row reaches the agent, not a web search", () => {
		// REGRESSION: this row used to carry `run: "ask <q>"`. No `ask` handler
		// exists in the registry, so the executor's pattern router found no
		// trigger and fell through to a WEB SEARCH. The intent now travels as a
		// typed kind, so nothing has to recover it by re-parsing a command string.
		const c = ctx({
			trimmed: "can you define gallop",
			// Ambiguous on its own → would otherwise be a fork card…
			inputDecision: NL("can you define gallop", false),
			completions: [
				comp({
					label: "Ask AI: can you define gallop",
					description: "can you define gallop",
					kind: "ask-ai",
				}),
			],
			completionIndex: 0,
		});
		// …but the explicit choice wins.
		expect(decideSubmit(c)).toEqual({ kind: "agent", prompt: "can you define gallop" });
	});

	it("a selected 'Search web' row runs the web handler", () => {
		const c = ctx({
			trimmed: "defuu",
			inputDecision: NL("defuu", false),
			completions: [comp({ label: "Search web: defuu", description: "defuu", kind: "search-web" })],
			completionIndex: 0,
		});
		expect(decideSubmit(c)).toEqual({ kind: "command", command: "web defuu" });
	});

	it("an Ask AI row carries no run string to be re-parsed", () => {
		// The defect class: a row whose meaning depends on downstream text
		// parsing. `description` holds the QUERY; `kind` holds the intent.
		const c = ctx({
			trimmed: "x",
			inputDecision: NL("x", false),
			completions: [comp({ label: "Ask AI: x", description: "x", kind: "ask-ai" })],
			completionIndex: 0,
		});
		expect(decideSubmit(c)).toEqual({ kind: "agent", prompt: "x" });
	});

	it("a selected correction WINS over the natural-language guard", () => {
		// REGRESSION: "can you define gallop" classifies as `nl`, and a correction
		// row carries no `run` — the shape the NL guard treats as "the question
		// owns Enter". That made selecting the suggestion silently open AI chat
		// instead of running the command the user just picked.
		const c = ctx({
			trimmed: "can you define gallop",
			inputDecision: NL("can you define gallop", false),
			completions: [
				comp({
					label: "Did you mean: define gallop?",
					description: "define gallop",
					kind: "correction",
				}),
			],
			completionIndex: 0,
		});
		expect(decideSubmit(c)).toEqual({ kind: "correct", value: "define gallop", run: true });
	});

	it("a SELECTED correction runs immediately; an auto-offered one only fills", () => {
		// Selecting the row is a decision already made — requiring a second Enter
		// was friction. An auto-offered guess (from the backend classifier on a
		// typo the user typed) still fills, so a wrong guess never self-executes.
		const selected = decideSubmit(
			ctx({
				trimmed: "can you define gallop",
				inputDecision: NL("can you define gallop", false),
				completions: [comp({ label: "x", description: "define gallop", kind: "correction" })],
				completionIndex: 0,
			}),
		);
		expect(selected).toEqual({ kind: "correct", value: "define gallop", run: true });

		const offered = decideSubmit(
			ctx({ trimmed: "weathr", inputDecision: { kind: "correct", corrected: "weather" } }),
		);
		expect(offered).toEqual({ kind: "correct", value: "weather" });
	});

	it("identifies a correction by its kind, not its label", () => {
		// A reworded label must still route correctly.
		const c = ctx({
			trimmed: "spoti",
			completions: [
				comp({
					label: "Perhaps you wanted open Spotify",
					description: "open Spotify",
					kind: "correction",
				}),
			],
			completionIndex: 0,
		});
		expect(decideSubmit(c)).toEqual({ kind: "correct", value: "open Spotify", run: true });
	});

	it("a tab-complete hint fills the input", () => {
		const c = ctx({
			trimmed: "tz",
			completions: [comp({ label: "tz tokyo", fill: "tz " })],
			completionIndex: 0,
		});
		expect(decideSubmit(c)).toEqual({ kind: "fill", value: "tz " });
	});

	it("a row with both fill and run EXECUTES on Enter", () => {
		// A multi-repo pick carries both: `fill` so Tab can refine the query,
		// `run` so Enter executes in the chosen repo. Checking `fill` first made
		// Enter re-fill the input — the row looked selected and did nothing.
		const action = decideSubmit(
			ctx({
				trimmed: "git status rturn-api",
				completions: [
					comp({
						label: "git status › rturn-api",
						fill: "git status rturn-api",
						run: "run git status @@/home/u/ws/rturn-api",
					}),
				],
				completionIndex: 0,
			}),
		);
		expect(action.kind).toBe("command");
		expect(action).toHaveProperty("command", "run git status @@/home/u/ws/rturn-api");
	});

	it("a row with only fill still fills", () => {
		// The tab-complete case must keep working: no `run` means there is
		// nothing to execute, so filling is the correct answer.
		const action = decideSubmit(
			ctx({
				trimmed: "tz",
				completions: [comp({ label: "tz tokyo", fill: "tz " })],
				completionIndex: 0,
			}),
		);
		expect(action.kind).toBe("fill");
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

	it("an ambiguous NL query (no runnable row selected) goes to the agent", () => {
		// Inline fallback rows were removed backend-side, so a bare NL query has no
		// competing row — the input decision drives Enter, straight to the agent.
		const c = ctx({
			trimmed: "pasta recipe",
			inputDecision: NL("pasta recipe"),
		});
		expect(decideSubmit(c)).toEqual({ kind: "agent", prompt: "pasta recipe" });
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

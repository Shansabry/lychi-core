import { describe, expect, it } from "vitest";
import type { CompletionItem } from "$lib/bindings";
import { defaultMatchIndex } from "$lib/defaultMatch";

function item(over: Partial<CompletionItem> & { label: string }): CompletionItem {
	return { score: 0, icon_path: null, ...over } as CompletionItem;
}

describe("defaultMatchIndex — the omnibox rule", () => {
	/**
	 * The regression that prompted extracting this. Typing `services` selected
	 * the "Session and Startup" app, so Enter launched an app instead of running
	 * the command. The app row is first and perfectly actionable — the ONLY thing
	 * disqualifying it is that it does not prefix-extend the typed text.
	 */
	it("does not select a row that fails to prefix-extend the input", () => {
		const results = [
			item({ label: "Session and Startup", run: "open Session and Startup" }),
			item({ label: "services" }),
		];
		expect(defaultMatchIndex(results, "services")).toBe(1);
	});

	/** The original bug the rule was written for. */
	it("does not select a longer history entry that merely contains the input", () => {
		const results = [item({ label: "run htop" }), item({ label: "run top" })];
		expect(defaultMatchIndex(results, "run top")).toBe(1);
	});

	it("selects a genuine prefix extension", () => {
		const results = [item({ label: "Firefox", run: "firefox" })];
		expect(defaultMatchIndex(results, "fir")).toBe(0);
	});

	it("returns -1 when nothing extends the input, so Enter runs it verbatim", () => {
		const results = [item({ label: "Session and Startup", run: "open Session and Startup" })];
		expect(defaultMatchIndex(results, "services")).toBe(-1);
	});

	it("is case-insensitive", () => {
		expect(defaultMatchIndex([item({ label: "Firefox", run: "Firefox" })], "fire")).toBe(0);
	});

	it("never defaults to a fallback row", () => {
		const results = [
			item({ label: "ask ai about services", kind: "ask-ai", run: "ask services" }),
			item({ label: "services" }),
		];
		expect(defaultMatchIndex(results, "services")).toBe(1);
	});

	it("skips decorative rows", () => {
		const results = [
			item({ label: "services", icon_path: "__separator__" }),
			item({ label: "services" }),
		];
		expect(defaultMatchIndex(results, "services")).toBe(1);
	});

	it("prefers `run` over `label`, matching what would actually execute", () => {
		// Displayed as a friendly label, but `run` is the real command — the test
		// must be against the command, or Enter runs something the predicate
		// never checked.
		const results = [item({ label: "Search YouTube: cats", run: "yt cats" })];
		expect(defaultMatchIndex(results, "yt")).toBe(0);
		expect(defaultMatchIndex(results, "Search")).toBe(-1);
	});

	it("returns -1 for empty or whitespace input", () => {
		const results = [item({ label: "services" })];
		expect(defaultMatchIndex(results, "")).toBe(-1);
		expect(defaultMatchIndex(results, "   ")).toBe(-1);
	});
});

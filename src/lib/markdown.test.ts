import { describe, expect, it } from "vitest";
import { repairStreamingMarkdown } from "./markdown";

describe("repairStreamingMarkdown", () => {
	it("closes an unclosed code fence so it doesn't swallow the rest", () => {
		const partial = "Here is code:\n```rust\nfn main() {";
		const fixed = repairStreamingMarkdown(partial);
		// The repaired copy has an even number of fences.
		expect((fixed.match(/^```/gm) ?? []).length % 2).toBe(0);
		expect(fixed).toContain("fn main() {");
	});

	it("leaves a properly-closed fence untouched", () => {
		const complete = "```js\nconst x = 1;\n```\ndone";
		expect(repairStreamingMarkdown(complete)).toBe(complete);
	});

	it("drops the dangling `](url` so no broken anchor is emitted", () => {
		const partial = "See [the docs](https://exa";
		const fixed = repairStreamingMarkdown(partial);
		// The `](partial-url` is removed until the closing paren arrives. The bare
		// `[the docs` that remains renders as literal text (marked makes no anchor
		// from an unclosed bracket) — safe, no malformed <a>.
		expect(fixed).toBe("See [the docs");
		expect(fixed).not.toContain("](");
	});

	it("keeps a complete link intact", () => {
		const complete = "See [the docs](https://example.com) now";
		expect(repairStreamingMarkdown(complete)).toBe(complete);
	});

	it("does not touch harmless dangling emphasis (marked renders it literally)", () => {
		const partial = "This is **very impor";
		expect(repairStreamingMarkdown(partial)).toBe(partial);
	});

	it("is a no-op on empty / plain text", () => {
		expect(repairStreamingMarkdown("")).toBe("");
		expect(repairStreamingMarkdown("just a sentence")).toBe("just a sentence");
	});
});

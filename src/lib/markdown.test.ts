import { describe, expect, it } from "vitest";
import { repairStreamingMarkdown, wrapTables } from "./markdown";

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

describe("wrapTables", () => {
	// Note: the full renderMarkdown path can't be unit-tested here — DOMPurify has
	// no callable `.sanitize` under jsdom/vitest — so we test the pure string
	// transform that does the wrapping.
	const table =
		"<table>\n<thead><tr><th>A</th></tr></thead>\n<tbody><tr><td>one</td></tr></tbody></table>";

	it("wraps a rendered table in a horizontal-scroll box", () => {
		const out = wrapTables(table);
		expect(out).toBe(`<div class="table-scroll">${table}</div>`);
	});

	it("wraps every table when there are several", () => {
		const out = wrapTables(`${table}\n<p>gap</p>\n${table}`);
		expect((out.match(/<div class="table-scroll">/g) ?? []).length).toBe(2);
		expect(out).toContain("<p>gap</p>");
	});

	it("leaves table-free html untouched", () => {
		const html = "<p>hello <strong>world</strong></p>";
		expect(wrapTables(html)).toBe(html);
	});
});

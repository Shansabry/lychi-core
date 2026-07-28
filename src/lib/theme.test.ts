import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { FONT_STACKS, fontStack } from "./theme";

// Read app.css from disk rather than importing it. `../app.css?raw` returns an
// empty string here — Vite's CSS pipeline intercepts the `?raw` suffix for
// stylesheets — and comparing against empty text would pass vacuously while
// detecting no drift at all.
const appCss = readFileSync(fileURLToPath(new URL("../app.css", import.meta.url)), "utf8");

// These cover the stack-building rules rather than the DOM write: the quoting,
// escaping and fallback-order decisions are where the real behaviour lives, and
// they are testable without a browser environment (the suite has no jsdom).

describe("font stack building", () => {
	it("prepends the chosen family rather than replacing the stack", () => {
		// The fallbacks must survive: if the font is later uninstalled, the
		// browser walks on to the next entry instead of dropping to the generic
		// family, which fontconfig often resolves to Bitstream Vera.
		const stack = fontStack("JetBrains Mono", FONT_STACKS.mono);
		expect(stack?.startsWith('"JetBrains Mono",')).toBe(true);
		expect(stack).toContain("DejaVu Sans Mono");
		expect(stack).toContain("monospace");
	});

	it("returns null when nothing is chosen, so the override is cleared", () => {
		expect(fontStack("", FONT_STACKS.sans)).toBeNull();
		expect(fontStack(undefined, FONT_STACKS.sans)).toBeNull();
	});

	it("treats whitespace as no choice", () => {
		expect(fontStack("   ", FONT_STACKS.sans)).toBeNull();
	});

	it("quotes family names containing spaces or punctuation", () => {
		// fontconfig really does report names like "Anka/Coder"; unquoted they
		// are invalid CSS and the entire declaration is discarded.
		expect(fontStack("Anka/Coder", FONT_STACKS.mono)).toContain('"Anka/Coder"');
		expect(fontStack("Noto Sans", FONT_STACKS.sans)).toContain('"Noto Sans"');
	});

	it("leaves a bare identifier unquoted", () => {
		expect(fontStack("Monospace", FONT_STACKS.mono)?.startsWith("Monospace,")).toBe(true);
	});

	it("escapes quotes and backslashes so a family name cannot inject CSS", () => {
		const stack = fontStack('Evil", color: red; x: "', FONT_STACKS.sans);
		// The injected quote must be escaped, leaving the payload inside the
		// string rather than terminating it and starting a new declaration.
		expect(stack).toContain('\\"');
		expect(stack?.startsWith('"Evil\\"')).toBe(true);
	});
});

describe("fallback stacks stay in sync with app.css", () => {
	// theme.ts must re-state the stacks, because setting a custom property
	// inline replaces the stylesheet's definition — CSS has no way for a
	// property to reference its own previous value. That duplication is exactly
	// the kind that drifts silently, so it is pinned here rather than trusted.

	it("actually read app.css", () => {
		// Guards the guard. An earlier attempt used `?raw`, which silently
		// returned "" — the comparisons below would then have compared against
		// nothing and passed while detecting no drift whatsoever.
		expect(appCss.length).toBeGreaterThan(0);
		expect(appCss).toContain("--font-mono:");
	});

	/** Families named in a CSS custom-property declaration, normalized. */
	function familiesIn(source: string, marker: string): string[] {
		const start = source.indexOf(marker);
		if (start === -1) return [];
		const end = source.indexOf(";", start);
		return source
			.slice(start + marker.length, end)
			.split(",")
			.map((s) => s.trim().replace(/^["']|["']$/g, ""))
			.filter(Boolean);
	}

	function familiesOf(stack: string): string[] {
		return stack.split(",").map((s) => s.trim().replace(/^["']|["']$/g, ""));
	}

	it("mono stack matches app.css", () => {
		expect(familiesOf(FONT_STACKS.mono)).toEqual(familiesIn(appCss, "--font-mono:"));
	});

	it("sans stack matches app.css", () => {
		expect(familiesOf(FONT_STACKS.sans)).toEqual(familiesIn(appCss, "--font-sans:"));
	});

	it("keeps a separate fixed-width stack for command output", () => {
		// The font picker retargets --font-sans and --font-mono. --font-output
		// must exist independently, or there would be nothing left holding
		// command output to a fixed-width face when a proportional font is
		// chosen — and `git status` would lose its columns.
		const output = familiesIn(appCss, "--font-output:");
		expect(output.length).toBeGreaterThan(0);
		expect(output.at(-1)).toBe("monospace");
	});
});

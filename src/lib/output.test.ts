import { describe, expect, it } from "vitest";
import type { CommandResult } from "$lib/ipc";
import { hasVisibleOutput, resolveOutput } from "$lib/output";

function res(over: Partial<CommandResult> = {}): CommandResult {
	return { success: true, duration_ms: 1, ...over } as CommandResult;
}

describe("resolveOutput — renderer", () => {
	it("defaults an untyped result to status", () => {
		expect(resolveOutput(res({ output: "Launched Firefox" })).renderer).toBe("status");
	});

	it("renders nothing when there is no output at all", () => {
		expect(resolveOutput(res()).renderer).toBe("none");
	});

	it.each([
		["terminal", "terminal"],
		["markdown", "markdown"],
		["svg", "svg"],
		["weather", "weather"],
		["text", "text"],
	] as const)("maps %s to its renderer", (type, renderer) => {
		expect(resolveOutput(res({ output: "x", output_type: type })).renderer).toBe(renderer);
	});
});

describe("resolveOutput — actions are derived, not hardcoded", () => {
	/**
	 * The reason this module exists. A terminal result used to be a dead end:
	 * inline runs are stdin-less and capped, so the user could read the output
	 * and do nothing with it.
	 */
	it("offers an escape to a real terminal for terminal output", () => {
		const { actions } = resolveOutput(res({ output: "…", output_type: "terminal" }));
		const ids = actions.map((a) => a.id);
		expect(ids).toContain("continue_in_terminal");
		expect(ids).toContain("rerun");
		expect(actions.find((a) => a.primary)?.id).toBe("continue_in_terminal");
	});

	/** Replaces the `isSvgResult` check that +page.svelte hardcoded. */
	it("offers copy-image for svg without the caller knowing the type", () => {
		const { actions } = resolveOutput(res({ output: "<svg/>", output_type: "svg" }));
		expect(actions.map((a) => a.id)).toContain("copy_image");
	});

	it("offers copy-as-markdown only for markdown", () => {
		const md = resolveOutput(res({ output: "# hi", output_type: "markdown" }));
		const txt = resolveOutput(res({ output: "hi", output_type: "text" }));
		expect(md.actions.map((a) => a.id)).toContain("copy_markdown");
		expect(txt.actions.map((a) => a.id)).not.toContain("copy_markdown");
	});

	it("offers copy-output whenever there is output, and never when there is not", () => {
		expect(resolveOutput(res({ output: "x" })).actions.map((a) => a.id)).toContain("copy_output");
		expect(resolveOutput(res()).actions).toHaveLength(0);
	});

	it("offers open-link only when the result carries a url", () => {
		const withUrl = resolveOutput(res({ output: "x", open_url: "https://example.com" }));
		expect(withUrl.actions.map((a) => a.id)).toContain("open_url");
		expect(resolveOutput(res({ output: "x" })).actions.map((a) => a.id)).not.toContain("open_url");
	});

	it("never yields duplicate action ids", () => {
		for (const t of ["terminal", "svg", "markdown", "text", "weather", undefined] as const) {
			const { actions } = resolveOutput(
				res({ output: "x", output_type: t, open_url: "https://e.com" }),
			);
			const ids = actions.map((a) => a.id);
			expect(new Set(ids).size).toBe(ids.length);
		}
	});
});

describe("hasVisibleOutput — what counts as something to show", () => {
	/**
	 * The bug this exists for. `services` returned 55 rows, but callers asked
	 * `success && !output` — and a `Rows` result leaves `output` empty because
	 * its payload is in `sections`. So a full list read as "bare success" and the
	 * launcher hid itself before rendering it.
	 */
	it("treats a rows result as visible even though `output` is empty", () => {
		const r = res({ sections: [{ title: null, rows: [], handler: "services" }] });
		expect(hasVisibleOutput(r)).toBe(true);
	});

	it("treats a bare success with no payload as nothing to show", () => {
		expect(hasVisibleOutput(res())).toBe(false);
	});

	it("treats plain text output as visible", () => {
		expect(hasVisibleOutput(res({ output: "done" }))).toBe(true);
	});
});

/**
 * Row titles are NOT unique, so nothing may use them as an identity.
 *
 * `dnf search firefox` legitimately returns three flatpak rows titled
 * "Firefox" (different remotes/versions), three "Nvidia VAAPI driver" and two
 * "Joplin". `RowsView` keyed its `{#each}` on `row.title`; Svelte 5 treats a
 * duplicate key as FATAL, so the render threw `each_key_duplicate`, the panel
 * painted nothing, and the spinner ran forever — while the backend had already
 * returned complete results in ~2.5s.
 *
 * There is no component-render harness in this project, so this asserts the
 * underlying fact rather than the rendering: duplicate titles in one section
 * are a normal, expected result shape. Any future code tempted to treat a
 * title as a key has to contend with this test.
 */
describe("row titles are not an identity", () => {
	it("a realistic package search yields duplicate titles in one section", () => {
		// Shape taken verbatim from a real `dnf search firefox` result.
		const titles = [
			"Firefox",
			"Firefox",
			"Firefox",
			"Add Water",
			"Nvidia VAAPI driver",
			"Nvidia VAAPI driver",
			"Joplin",
			"Joplin",
		];
		const unique = new Set(titles);
		expect(unique.size).toBeLessThan(titles.length);
	});
});

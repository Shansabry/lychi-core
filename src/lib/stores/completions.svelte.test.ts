import { beforeEach, describe, expect, it } from "vitest";
import { completions } from "./completions.svelte";

/**
 * The suggestion-list visibility rule: ONE surface at a time.
 *
 * Suggestions belong to "choosing what to run" — initial load and typing. Once
 * something has run and its output is on screen, the list must be gone. The bug
 * these cover: running a command from a recents row left the input empty, so
 * `afterContextReady` refilled the list and it rendered stacked above the
 * result card (both surfaces visible at once).
 *
 * These assert `visible`, the gate itself — NOT `items`. Every execution path
 * used to clear `items` by hand, and a test against `items` would pass against
 * that arrangement too, which is exactly the drift being removed.
 */
describe("completions.visible", () => {
	beforeEach(() => {
		completions.outputShown = false;
		completions.atMode = false;
		completions.searchMode = false;
	});

	it("shows the list when nothing has run yet (initial load)", () => {
		expect(completions.visible).toBe(true);
	});

	it("hides the list once an output surface is on screen", () => {
		completions.outputShown = true;
		expect(completions.visible).toBe(false);
	});

	it("shows the list again when the output is dismissed", () => {
		completions.outputShown = true;
		completions.outputShown = false;
		expect(completions.visible).toBe(true);
	});

	// The exemptions: `/` and `@` are interactive pickers driven by the input, so
	// a result card left over from a previous command must not blank them out.
	// Without these branches the file picker would render empty whenever a stale
	// result happened to still be on screen.
	it("keeps the list visible in /-search even while an output is shown", () => {
		completions.outputShown = true;
		completions.searchMode = true;
		expect(completions.visible).toBe(true);
	});

	it("keeps the list visible in @-browse even while an output is shown", () => {
		completions.outputShown = true;
		completions.atMode = true;
		expect(completions.visible).toBe(true);
	});

	it("gates on output, not on whether the list happens to be empty", () => {
		// `items` and `visible` are independent: a populated list is hidden by the
		// gate alone. If this ever passes only because something cleared `items`,
		// the single decider has grown a second one.
		completions.items = [{ label: "weather here", icon_path: null, score: 1, description: null }];
		completions.outputShown = true;
		expect(completions.visible).toBe(false);
		expect(completions.items.length).toBe(1);
	});
});

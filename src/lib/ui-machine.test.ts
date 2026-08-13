import { describe, expect, it } from "vitest";
import {
	aiParked,
	anyPanelOpen,
	clearAi,
	closePanel,
	INITIAL,
	openPanel,
	type Panel,
	parkAi,
	reset,
	restoreAi,
	showAi,
	showAiSurface,
	showResults,
	togglePanel,
	type UiSnapshot,
} from "./ui-machine";

/** A snapshot builder for readable tests. */
function snap(over: Partial<UiSnapshot> = {}): UiSnapshot {
	return { surface: "results", panel: "none", aiExists: false, ...over };
}

const ALL_PANELS: Panel[] = ["history", "chat-history", "notes", "media", "settings"];

describe("ui-machine — the anti-overlap invariant (the presenting bug)", () => {
	it("opening Settings while an AI answer is on screen hides the answer, no overlap", () => {
		// Reproduce the exact bug: AI answer is the surface, then open Settings.
		const withAi = showAiSurface(INITIAL); // { surface: "ai", aiExists: true }
		const s = openPanel(withAi, "settings");

		// Settings is visible …
		expect(s.panel).toBe("settings");
		expect(anyPanelOpen(s)).toBe(true);
		// … and the AI answer is NOT visible (no overlap) …
		expect(showAi(s)).toBe(false);
		// … but it is preserved and recallable (parked).
		expect(aiParked(s)).toBe(true);
	});

	it("AI-visible and any-panel-visible are mutually exclusive for every panel", () => {
		const withAi = showAiSurface(INITIAL);
		for (const p of ALL_PANELS) {
			const s = openPanel(withAi, p);
			expect(showAi(s) && anyPanelOpen(s)).toBe(false);
		}
	});

	it("exactly one main surface is showable at a time (never two)", () => {
		for (const surface of ["results", "ai"] as const) {
			const s = snap({ surface, aiExists: surface === "ai" });
			const shown = [showResults(s), showAi(s)].filter(Boolean).length;
			// results/ai each show exactly themselves; never both.
			expect(shown).toBeLessThanOrEqual(1);
		}
	});
});

describe("ui-machine — panel transitions", () => {
	it("closePanel returns to the parked AI answer if one exists", () => {
		const parked = openPanel(showAiSurface(INITIAL), "notes");
		expect(aiParked(parked)).toBe(true);
		const s = closePanel(parked);
		expect(s.panel).toBe("none");
		expect(showAi(s)).toBe(true); // the parked answer comes back
	});

	it("closePanel returns to results when no AI answer exists", () => {
		const s = closePanel(openPanel(INITIAL, "settings"));
		expect(showResults(s)).toBe(true);
	});

	it("togglePanel opens then closes the same panel", () => {
		const opened = togglePanel(INITIAL, "media");
		expect(opened.panel).toBe("media");
		const closed = togglePanel(opened, "media");
		expect(closed.panel).toBe("none");
	});

	it("togglePanel switches directly between two panels", () => {
		const s = togglePanel(openPanel(INITIAL, "history"), "settings");
		expect(s.panel).toBe("settings");
	});
});

describe("ui-machine — AI parking lifecycle", () => {
	it("parkAi from the AI surface keeps the answer, returns to results", () => {
		const s = parkAi(showAiSurface(INITIAL));
		expect(showResults(s)).toBe(true);
		expect(aiParked(s)).toBe(true); // still exists, just off-stage
	});

	it("parkAi is a no-op when AI is not the current surface", () => {
		const onResults = snap({ aiExists: true }); // answer exists but parked already
		expect(parkAi(onResults)).toEqual(onResults);
	});

	it("restoreAi brings a parked answer back to the surface", () => {
		const parked = openPanel(showAiSurface(INITIAL), "settings");
		const s = restoreAi(parked);
		expect(showAi(s)).toBe(true);
		expect(s.panel).toBe("none");
	});

	it("clearAi removes the answer entirely", () => {
		const s = clearAi(showAiSurface(INITIAL));
		expect(s.aiExists).toBe(false);
		expect(aiParked(s)).toBe(false);
		expect(showResults(s)).toBe(true);
	});

	it("showAiSurface closes any open panel", () => {
		const s = showAiSurface(openPanel(INITIAL, "settings"));
		expect(s.panel).toBe("none");
		expect(showAi(s)).toBe(true);
	});
});

describe("ui-machine — reset", () => {
	it("reset returns a pristine empty launcher", () => {
		const messy = openPanel(showAiSurface(INITIAL), "settings");
		const s = reset();
		expect(s).toEqual(INITIAL);
		expect(aiParked(s)).toBe(false);
		expect(anyPanelOpen(messy)).toBe(true); // sanity: reset() ignores prior state
	});
});

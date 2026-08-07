import { describe, expect, it } from "vitest";
import { ui } from "./ui.svelte";

/**
 * D2: `ui.svelte.ts` used to declare eight fields, seven of which were shadowed
 * by page-local `$state` and therefore had zero readers. The live bug that
 * caused: `summonReset()` set `this.actionPanelOpen = false` on the dead copy
 * while the panel rendered from the page-local one, so Ctrl+K → dismiss →
 * re-summon left the action panel open — with a doc comment claiming otherwise.
 *
 * These assert the reset CONTRACT rather than the field plumbing, so they keep
 * holding if the panel flag moves again, and they fail if a future summon path
 * forgets to clear it.
 */
describe("ui.summonReset", () => {
	it("closes an open action panel", () => {
		ui.actionPanelOpen = true;
		ui.summonReset();
		expect(ui.actionPanelOpen).toBe(false);
	});

	it("returns to the pristine launcher surface", () => {
		ui.openPanel("media");
		ui.summonReset();
		expect(ui.anyPanelOpen).toBe(false);
		expect(ui.resultsVisible).toBe(true);
	});

	it("leaves nothing parked", () => {
		ui.showAi();
		ui.parkAi();
		ui.summonReset();
		expect(ui.aiExists).toBe(false);
		expect(ui.aiParked).toBe(false);
		expect(ui.aiVisible).toBe(false);
	});

	it("is idempotent — a second summon changes nothing", () => {
		ui.actionPanelOpen = true;
		ui.openPanel("media");
		ui.summonReset();
		const after = { ...ui.snapshot, actionPanelOpen: ui.actionPanelOpen };
		ui.summonReset();
		expect({ ...ui.snapshot, actionPanelOpen: ui.actionPanelOpen }).toEqual(after);
	});
});

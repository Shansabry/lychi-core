/**
 * UiState — the reactive shell around the pure surface machine (`$lib/ui-machine`).
 *
 * This is the single source of truth for what's visible in the launcher. It holds
 * ONE `UiSnapshot` in `$state` and exposes the machine's predicates as reactive
 * getters and its transitions as methods. Because visibility is derived from one
 * snapshot (two enums + a flag), two surfaces can never be visible at once — the
 * overlap bug is structurally impossible (see ui-machine.ts for the invariant).
 *
 * Also owns the action-panel flag — the one piece of transient chrome with no
 * transition of its own, kept here because `summonReset` must clear it.
 *
 * It deliberately does NOT mirror window/layout/onboarding/flash state. Those
 * were declared here once and shadowed by page-local `$state`, so the copies
 * here had zero readers while looking authoritative — and `summonReset` cleared
 * an action-panel flag nothing rendered. A field belongs here only if this is
 * the copy that renders.
 *
 * Svelte 5 note: transitions are arrow-function fields so `this` is bound when they
 * are passed down as component callback props (`onclick={ui.closePanel}`).
 */

import * as m from "$lib/ui-machine";

class UiState {
	// The one snapshot everything derives from.
	snapshot = $state<m.UiSnapshot>({ ...m.INITIAL });

	// --- Transient chrome owned here because `summonReset` must clear it ---
	/**
	 * Whether the ⌘K per-result action panel is open.
	 *
	 * Lives here rather than in the page because summon has to close it, and a
	 * page-local copy meant `summonReset` cleared a field nobody rendered: the
	 * panel stayed open across dismiss → re-summon while this said otherwise.
	 * The page reads and writes THIS field; there is no second copy.
	 */
	actionPanelOpen = $state(false);
	/** Local-AI model warmup ("Loading AI model…" status-bar indicator). */
	aiLoading = $state(false);

	// --- Derived visibility (what the template reads) ---
	get anyPanelOpen(): boolean {
		return m.anyPanelOpen(this.snapshot);
	}
	get aiVisible(): boolean {
		return m.showAi(this.snapshot);
	}
	get resultsVisible(): boolean {
		return m.showResults(this.snapshot);
	}
	/** An AI answer exists but is off-stage → status-bar recall icon. */
	get aiParked(): boolean {
		return m.aiParked(this.snapshot);
	}
	/** An AI answer exists at all (on-stage OR parked). */
	get aiExists(): boolean {
		return this.snapshot.aiExists;
	}
	/** Whether the given panel is the open one (feeds class:panel-hidden + `visible`). */
	panelVisible = (p: m.Panel): boolean => m.panelVisible(this.snapshot, p);

	// --- Transitions (the ONLY way visibility changes) ---
	openPanel = (p: m.Panel): void => {
		this.snapshot = m.openPanel(this.snapshot, p);
	};
	closePanel = (): void => {
		this.snapshot = m.closePanel(this.snapshot);
	};
	togglePanel = (p: m.Panel): void => {
		this.snapshot = m.togglePanel(this.snapshot, p);
	};
	showAi = (): void => {
		this.snapshot = m.showAiSurface(this.snapshot);
	};
	parkAi = (): void => {
		this.snapshot = m.parkAi(this.snapshot);
	};
	restoreAi = (): void => {
		this.snapshot = m.restoreAi(this.snapshot);
	};
	clearAi = (): void => {
		this.snapshot = m.clearAi(this.snapshot);
	};
	showPlan = (): void => {
		this.snapshot = m.showPlanSurface(this.snapshot);
	};
	/** Force the plain launcher surface (typing a fresh query). */
	showLauncher = (): void => {
		this.snapshot = m.showLauncher(this.snapshot);
	};
	/** Full reset on summon: pristine launcher, nothing parked, action panel closed. */
	summonReset = (): void => {
		this.snapshot = m.reset();
		this.actionPanelOpen = false;
	};
}

/** The single app-wide UI state instance. */
export const ui = new UiState();

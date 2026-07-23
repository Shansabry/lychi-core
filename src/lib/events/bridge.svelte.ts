/**
 * Tauri event bridge — the ONE place that subscribes to `lychi://` events and the
 * window keydown/move. It's the transport layer: store-only events go straight to
 * the pure `route.*` handlers (`events/router.ts`); the page-coupled events
 * (summon reset/focus, dismiss-hide, escape, window-move, and the pieces of the
 * store events that also drive page orchestration) are delegated back through the
 * `BridgeHandlers` callback surface the page supplies.
 *
 * This empties +page.svelte's onMount of its ~200-line listener block: the page
 * now calls `attachTauriEvents(handlers)` once and disposes the returned teardown.
 * No logic lives here beyond wiring — behaviour is preserved from the old inline
 * listeners verbatim.
 */

import { getCurrentWindow } from "@tauri-apps/api/window";
import { handleWindowKey, type WindowKeyEffects } from "$lib/commands/dispatch";
import type {
	AgentEventDto,
	EnvironmentContext,
	FileSearchBatch,
	StepEvent,
	TrackInfo,
} from "$lib/ipc";
import { route } from "./router";

/** The page-coupled effects the bridge delegates back to +page.svelte. */
export interface BridgeHandlers {
	/** Mark the launcher visible/ready (shown / summon / self-heal). */
	ready: () => void;
	/** A finished agent-plan step (drives the plan panel + last result). */
	agentStep: (payload: StepEvent) => void;
	/** Full summon reset (input/plan/routing clear, focus, load suggestions…). */
	summon: () => void;
	/** After fresh context arrives, re-enrich empty-input suggestions. */
	afterContextReady: () => void;
	/** Blur-dismiss (respects hide-on-blur + open-panel-closes-first). */
	dismiss: () => void;
	/** GTK-level Escape (focus-independent). */
	escape: () => void;
	/** Live AI availability (from the AiReactor). Keeps `aiEnabled` in sync so
	 *  NL/`ask` routing switches between agent and web the instant the provider
	 *  is enabled/disabled — no polling, no stale flag. */
	aiStatusChanged: (available: boolean) => void;
	/** Persist the moved window position (already debounced by the bridge). */
	windowMoved: (x: number, y: number) => void;
	/** Effects for the capture-phase window keydown dispatcher. */
	keyEffects: WindowKeyEffects;
}

/**
 * Attach every `lychi://` subscription + the window keydown/move. Returns a
 * disposer that removes them all. No-op (returns a noop disposer) outside Tauri.
 */
export async function attachTauriEvents(h: BridgeHandlers): Promise<() => void> {
	if (!("__TAURI_INTERNALS__" in window)) return () => {};

	const win = getCurrentWindow();
	const offs: (() => void)[] = [];

	// Self-heal: if the window was already mapped before we attached (cold start —
	// summon/shown fired into the void), become ready now.
	win
		.isVisible()
		.then((visible) => {
			if (visible) h.ready();
		})
		.catch(() => {});

	// --- Store-only events → pure router ---
	offs.push(
		await win.listen<AgentEventDto>("lychi://agent-event", (e) => route.agentEvent(e.payload)),
	);
	offs.push(await win.listen("lychi://context-stale", () => route.contextStale()));
	offs.push(
		await win.listen<EnvironmentContext>("lychi://context-ready", (e) => {
			route.contextReady(e.payload);
			h.afterContextReady();
		}),
	);
	offs.push(await win.listen<TrackInfo>("lychi://media-track", (e) => route.mediaTrack(e.payload)));
	offs.push(await win.listen<string>("lychi://ai-load-state", (e) => route.aiLoadState(e.payload)));
	offs.push(
		await win.listen<FileSearchBatch>("lychi://file-search-results", (e) =>
			route.fileSearch(e.payload),
		),
	);

	// --- Page-coupled events → delegated handlers ---
	offs.push(await win.listen<StepEvent>("lychi://agent-step", (e) => h.agentStep(e.payload)));
	offs.push(await win.listen("lychi://shown", () => h.ready()));
	offs.push(await win.listen("lychi://summon", () => h.summon()));
	offs.push(await win.listen("lychi://dismiss", () => h.dismiss()));
	offs.push(await win.listen("lychi://gtk-escape", () => h.escape()));
	offs.push(
		await win.listen<boolean>("lychi://ai-status-changed", (e) => h.aiStatusChanged(e.payload)),
	);

	// Persist window position on move (debounced). Skip (0,0) — Wayland doesn't
	// report real positions.
	let moveTimer: ReturnType<typeof setTimeout> | undefined;
	offs.push(
		await win.onMoved(({ payload: pos }) => {
			if (pos.x === 0 && pos.y === 0) return;
			clearTimeout(moveTimer);
			moveTimer = setTimeout(() => h.windowMoved(pos.x, pos.y), 500);
		}),
	);

	// Capture-phase window keydown for focus-independent shortcuts.
	const onKeyDown = (e: KeyboardEvent) => handleWindowKey(e, h.keyEffects);
	window.addEventListener("keydown", onKeyDown, true);
	offs.push(() => window.removeEventListener("keydown", onKeyDown, true));

	return () => {
		for (const off of offs) off();
	};
}

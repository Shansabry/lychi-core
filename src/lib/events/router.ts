/**
 * Pure event router — maps store-only `lychi://` events to store mutations.
 *
 * These handlers touch ONLY the stores (no Tauri, no page-local state, no DOM), so
 * they're node-testable: construct/inspect the store singletons, call
 * `route.agentEvent(payload)`, assert. The transport that actually subscribes to
 * `win.listen` lives in `events/bridge.svelte.ts`; the page-coupled events (summon,
 * dismiss, escape, keyboard, window-move) stay in the bridge's callback surface
 * because they drive page orchestration a store can't own.
 *
 * Keeping this seam pure is the Phase-5 payoff: the streaming/agent/context/media
 * event application is verifiable without a browser or the Tauri runtime.
 */

import type { AgentEventDto, EnvironmentContext, FileSearchBatch, TrackInfo } from "$lib/ipc";
import { chat } from "$lib/stores/chat.svelte";
import { completions } from "$lib/stores/completions.svelte";
import { context } from "$lib/stores/context.svelte";
import { media } from "$lib/stores/media.svelte";
import { ui } from "$lib/stores/ui.svelte";

export const route = {
	/** `lychi://agent-event` — the tool-calling coordinator stream. */
	agentEvent: (ev: AgentEventDto): void => chat.applyEvent(ev),

	/** `lychi://context-stale` — a background re-gather started. */
	contextStale: (): void => context.onStale(),

	/** `lychi://context-ready` — adopt the fresh context. Returns void; the bridge
	 *  also re-runs `loadEmptySuggestions` afterward (page-coupled). */
	contextReady: (env: EnvironmentContext): void => context.onReady(env),

	/** `lychi://media-track` — upsert one MPRIS player. */
	mediaTrack: (track: TrackInfo): void => media.applyTrack(track),

	/** `lychi://ai-load-state` — local-model warmup indicator ("loading"|else). */
	aiLoadState: (state: string): void => {
		ui.aiLoading = state === "loading";
	},

	/** `lychi://file-search-results` — apply one streamed search batch. */
	fileSearch: (batch: FileSearchBatch): void => completions.applyFileSearchBatch(batch),
};

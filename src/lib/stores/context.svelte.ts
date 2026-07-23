/**
 * ContextState — the active-window/cwd/git environment context and its freshness.
 *
 * Extracted from +page.svelte (Phase 4). Holds the gathered `EnvironmentContext`,
 * the staleness indicator (a dim status-bar glyph, set from a `__context_stale__`
 * completions sentinel), the "actively re-gathering" flag (between the
 * `context-stale` and `context-ready` events), and the delayed-loading skeleton
 * flag. Derives the terminal/IDE `contextPill` shown in the input.
 *
 * `loadingTimer` is a plain non-reactive field (a timer handle).
 */

import type { CompletionItem, EnvironmentContext } from "$lib/ipc";

class ContextState {
	/** The gathered environment (active window, cwd, git). Null until first ready. */
	env = $state<EnvironmentContext | null>(null);

	/** Context is outdated (idle "context outdated" bulb). Set from a completions sentinel. */
	stale = $state(false);
	staleHint = $state("");

	/** A background re-gather is in flight (the "updating context…" spinner). */
	refreshing = $state(false);

	/** Delayed skeleton while the first gather is pending. */
	loading = $state(false);

	/** Non-reactive delayed-loading timer handle. */
	loadingTimer: ReturnType<typeof setTimeout> | undefined;

	/**
	 * The terminal/IDE pill text (folder · branch), or "" when the focused window
	 * is neither a terminal nor an IDE (we never show a pill for browsers/etc.).
	 */
	get pill(): string {
		const w = this.env?.active_window;
		if (!w?.is_terminal && !w?.is_ide) return "";
		const ideIsFocused = w?.is_ide ?? false;
		const cwd = ideIsFocused
			? (this.env?.cwd ?? this.env?.terminal_cwd)
			: (this.env?.terminal_cwd ?? this.env?.cwd);
		const folder = cwd?.split("/").pop();
		if (!folder) return "";
		const branch = this.env?.git?.branch;
		return branch ? `${folder} · ${branch}` : folder;
	}

	/**
	 * Pull a `__context_stale__` sentinel row out of a completions result: set the
	 * staleness indicator from it and return the results with the sentinel removed
	 * (or unchanged + indicator cleared when absent).
	 */
	extractStale = (results: CompletionItem[]): CompletionItem[] => {
		const staleRow = results.find((c) => c.icon_path === "__context_stale__");
		this.stale = !!staleRow;
		this.staleHint = staleRow?.description ?? "";
		return staleRow ? results.filter((c) => c.icon_path !== "__context_stale__") : results;
	};

	/** `lychi://context-stale` — a re-gather started. */
	onStale = (): void => {
		this.refreshing = true;
	};

	/** `lychi://context-ready` — the re-gather finished; adopt the new context. */
	onReady = (env: EnvironmentContext): void => {
		this.env = env;
		this.refreshing = false;
		clearTimeout(this.loadingTimer);
		this.loading = false;
	};

	/** Clear context on summon; arm the delayed-loading skeleton. */
	reset = (): void => {
		this.env = null;
		clearTimeout(this.loadingTimer);
		this.loading = false;
		this.loadingTimer = setTimeout(() => {
			this.loading = true;
		}, DELAYED_SKELETON_MS);
	};
}

/** How long to wait before showing the context-loading skeleton (>120ms). */
const DELAYED_SKELETON_MS = 120;

/** The single app-wide context state. */
export const context = new ContextState();

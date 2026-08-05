/**
 * Completions — the completion list, `@`-browse, and `/`-file-search state.
 *
 * Extracted from +page.svelte (Phase 3 of the FE re-architecture). This is the
 * most timing-sensitive cluster in the app: results arrive asynchronously (IPC
 * promises + a streaming `lychi://file-search-results` event) and must never let
 * a stale run clobber a fresher one.
 *
 * CRITICAL — the async staleness guards are plain NON-reactive fields, never
 * `$state`:
 *   - `completionGen` — bumped on every new completions/@ query; a late promise
 *     whose captured gen != the current one is dropped.
 *   - `fileSearchId`  — bumped per `/`-search; a `file-search-results` batch with a
 *     stale id is ignored.
 *   - `debounceTimer` — the pending setTimeout handle.
 * Making any of these reactive would re-trigger effects on every bump (and is
 * pointless — nothing renders them). Do NOT add `$state` to them.
 *
 * The `filePathMap`/`fileMetaMap` are reassigned WHOLESALE (`new Map(...)`), never
 * mutated in place — that reassignment is what makes them reactive. Keep that.
 *
 * The `$derived` breadcrumbs that also read the launcher's `inputValue`
 * (`searchPathContext` for `/`, `atPathContext` for `@`) stay in +page.svelte —
 * they're view-glue over page-owned input, not part of this cluster's state.
 */

import type { CompletionItem, FileSearchBatch, MountPoint } from "$lib/ipc";

type FileMeta = { size_bytes?: number | null; modified_secs?: number | null };

class Completions {
	// The rendered completion list + selected row (-1 = none).
	items = $state.raw<CompletionItem[]>([]);
	index = $state(-1);
	/** A query is in flight (drives the cold-start skeleton). */
	pending = $state(false);

	// `@` file-reference (browse — point to a file in a command).
	atMode = $state(false);
	atStart = $state(-1);
	atNoResults = $state(false);

	// `/` file search (find + open a file, Spotlight-style).
	searchMode = $state(false);
	searchDone = $state(false);
	mountPoints = $state.raw<MountPoint[]>([]);
	scopeIndex = $state(0);
	/** Absolute path when drilled into a folder within `/`-search. */
	searchScopePath = $state("");
	/** label → full_path (for preview) and label → metadata. Reassigned wholesale. */
	filePathMap = $state<Map<string, string>>(new Map());
	fileMetaMap = $state<Map<string, FileMeta>>(new Map());
	/** The current search hit .gitignore rules (shows an indicator). */
	ignoreActive = $state(false);

	// --- Non-reactive async guards (see file header) ---
	completionGen = 0;
	fileSearchId = 0;
	debounceTimer: ReturnType<typeof setTimeout> | undefined;

	/** The active `/`-search scope path (home by default). */
	get activeScope(): string {
		return this.mountPoints[this.scopeIndex]?.path ?? "";
	}

	/**
	 * An output is on screen (a result card, an AI answer, a plan). The launcher
	 * shows ONE surface at a time: suggestions while you're choosing what to run,
	 * output once something has run.
	 *
	 * Set by the page from whatever it is currently rendering, and read back here
	 * by `visible` — so "is the list allowed to show" is decided in one place
	 * rather than by each execution path remembering to clear `items`. Those
	 * ~30 scattered `completions.items = []` calls were that missing rule spelled
	 * out once per caller, which is why the paths that forgot (a recents row
	 * running a command, then `afterContextReady` refilling the list under the
	 * result) showed both surfaces at once.
	 */
	outputShown = $state(false);

	/**
	 * Whether the completion list may render. The ONE gate.
	 *
	 * `/`-search and `@`-browse are exempt: they're interactive pickers driven by
	 * the input, so a stale result behind them must not blank them out.
	 */
	get visible(): boolean {
		if (this.atMode || this.searchMode) return true;
		return !this.outputShown;
	}

	/** Replace the list + reset selection (common little operation). */
	setItems = (items: CompletionItem[], index = -1): void => {
		this.items = items;
		this.index = index;
	};

	/** Clear everything back to an empty, non-searching state. */
	clear = (): void => {
		this.items = [];
		this.index = -1;
		this.pending = false;
	};

	/** Leave `/`-search mode, dropping its per-search maps/flags. */
	exitSearch = (): void => {
		this.searchScopePath = "";
		this.searchMode = false;
		this.filePathMap = new Map();
		this.fileMetaMap = new Map();
		this.ignoreActive = false;
	};

	/**
	 * Apply one streaming `/`-search batch. Encapsulates the stale-id guard, the
	 * wholesale-Map clone+reassign, the rAF deferral (so a batch never blocks a
	 * keystroke paint), and the REPLACE-with-dedup ranking contract (each batch is
	 * the full ranked snapshot; dedup by full_path, preserve backend order, keep
	 * section headers). Behaviour is a verbatim move from +page.svelte.
	 */
	applyFileSearchBatch = (batch: FileSearchBatch): void => {
		if (batch.search_id !== this.fileSearchId) return; // stale search — ignore

		const pathUpdates = new Map(this.filePathMap);
		const metaUpdates = new Map(this.fileMetaMap);
		for (const r of batch.results) {
			pathUpdates.set(r.label, r.full_path);
			metaUpdates.set(r.label, { size_bytes: r.size_bytes, modified_secs: r.modified_secs });
		}

		// A section-header row (backend tags description "__separator__") renders as a
		// centered non-selectable label; everything else is a file/folder result.
		const newItems: CompletionItem[] = batch.results.map((r) => {
			const isSep = r.description === "__separator__";
			return {
				label: r.label,
				icon_path: isSep ? "__separator__" : r.is_dir ? "__folder__" : null,
				score: r.score,
				description: isSep ? null : (r.description ?? null),
			};
		});

		const isDone = batch.done;

		// Defer state update to next frame so it never blocks a keystroke paint.
		requestAnimationFrame(() => {
			this.filePathMap = pathUpdates;
			this.fileMetaMap = metaUpdates;
			if (batch.has_ignore_rules) this.ignoreActive = true;

			// Each batch carries the FULL ranked snapshot for this search (the index
			// re-emits the whole top-N as it fills), already ordered by the backend —
			// REPLACE, don't append (appending re-adds paths every emit → duplicates).
			// Dedup by full_path as a guard; preserve the backend's order (no re-sort).
			// Section headers (empty full_path) are kept verbatim — never deduped.
			const seen = new Set<string>();
			this.items = newItems
				.filter((item) => {
					if (item.icon_path === "__separator__") return true;
					const key = pathUpdates.get(item.label) ?? item.label;
					if (seen.has(key)) return false;
					seen.add(key);
					return true;
				})
				.slice(0, 20);

			// Auto-select the first SELECTABLE row (skip a leading section header).
			if (this.index < 0) {
				const first = this.items.findIndex((c) => c.icon_path !== "__separator__");
				if (first >= 0) this.index = first;
			}

			if (isDone) {
				this.searchDone = true;
				this.atNoResults = this.items.length === 0;
			}
		});
	};
}

/** The single app-wide completions/search state. */
export const completions = new Completions();

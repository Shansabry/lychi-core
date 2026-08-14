# File search: "Searching…" sticks on the first query (cold start)

**Status:** Known bug, deferred. Diagnosed, not yet fixed.
**Severity:** UX rough edge — first-run only, self-resolves when the user types another character.
**Not a regression from the storage refactor** (see "Provenance").

## Symptom

The first `/`-file-search query after launch (or after the search subsystem has
gone idle) shows **"Searching…"** and never populates, even though results exist.
Typing one more character makes results appear instantly. Reproducible by
launching cold and immediately typing e.g. `/Don` — the list stays on
"Searching…" until the next keystroke, which returns the folder results.

## Root cause: loading-vs-empty state conflation

This is a textbook async-search bug (Algolia documents the exact shape: an init
phase that emits an empty result set with an `idle` status, which the UI misreads
as "done, found nothing"). Lychi's chain:

1. The file-search backend is **asynchronous** — `fuzzy_path_completions` starts a
   `LiveSearch` session and returns "**whatever has matched so far**" (see
   `crates/lychi-core/src/file_search/mod.rs`, the one-shot comment: *"reads
   whatever is ready. Correct rather than complete."*).
2. On the **first query against a cold index/matcher**, the session hasn't matched
   anything yet → it returns an **empty** set.
3. The UI treats "empty so far" as a **terminal** state and latches "Searching…"
   (there is no `done` signal on the one-shot path to say the search actually
   finished), so it never transitions to results.
4. It's stuck because the query fired once (debounced) and nothing re-fires until
   the next keystroke — which hits a now-**warm** index and returns results.

**Why first-run-only:** the matcher/corpus is aggressively released when idle
(recent perf commits: `e5febfa release the search matcher when searching goes
idle`, `61793a2 evict path corpora for scopes nobody is searching`,
`ad759d9 break the Arc cycle`). So the first search after launch/idle is cold; all
subsequent ones are warm.

## Provenance (NOT the storage refactor)

- `src/routes/+page.svelte` and `src/lib/stores/completions.svelte.ts` were **not
  modified** in the storage-refactor session.
- The frecency move made `frecency::get_scores()` return empty on a fresh db (a
  separate, now-fixed bug — see the `init_store` table-creation fix), but that only
  affects **ranking**, called AFTER the results check — it cannot cause an empty
  *result set* or a stuck spinner.
- The cold-start predates this session, rooted in the idle-eviction perf work.

## The current frontend is mid-migration (the real blocker to a clean fix)

A **streaming** result path exists but appears **half-wired**:

- `lychi://file-search-results` IS listened (`src/lib/events/bridge.svelte.ts`)
  → routes to `completions.applyFileSearchBatch` (`src/lib/events/router.ts`).
- `applyFileSearchBatch` already handles `search_id` staleness and a `done` flag
  (sets `searchDone = true`) — the correct machinery.
- BUT: `startFileSearch` (`src/lib/ipc.ts`) is **never called** in live code;
  `completions.searchMode` is **only ever set to `false`** (never `true`);
  `completions.fileSearchId` is **never incremented**. So the streaming entry path
  (enter search mode → bump id → invoke startFileSearch → receive batches) is not
  driven.
- The live `/`-search therefore runs through a **different/older path** (the
  one-shot `fuzzy_path_completions`, or another trigger not yet located by static
  reading). That one-shot path is the one with no `done` signal → the stuck
  spinner.

Pinning the exact live trigger needs the app running with browser devtools (real
stack + call trace) or a full read of the large `+page.svelte` + store, not more
grep.

## Recommended fix (researched, industry-standard)

Primary — **make the streaming `done`/`search_id` contract the single source of
truth** (what Telescope, VS Code Quick Open, and Algolia effectively do):

1. Route `/`-search through the streaming path (invoke `startFileSearch`, bump
   `fileSearchId` per query, enter `searchMode`).
2. Bind UI state to the stream, not to any one-shot return, for the current
   `search_id`:
   - **"Searching…" while `!done`.**
   - **Populate on any non-empty batch.**
   - **"No results" ONLY when a batch with `done: true` arrives empty** — never on
     an interim empty batch.
3. **Ignore batches whose `search_id` isn't the latest** (stale-session guard —
   already present in `applyFileSearchBatch`).
4. **Delay the "Searching…" indicator ~150–200ms** so fast warm queries never flash
   it (Algolia's `stalledSearchDelay` default is 200ms).

Complementary — **warm the file-search index/matcher at startup** (background pass)
so the first real query is rarely cold. This shrinks the race window but does NOT
close it (the user can type before warmup finishes), so it is a latency
optimization on top of the done-signal fix, not a standalone fix.

Avoid:
- **Client re-poll on empty** — the Alfred pattern for a *one-shot* backend;
  redundant polling on top of an existing streaming channel.
- **"Searching…" timing out to "No results"** as the primary fix — a blind timeout
  can declare "No results" while a slow-but-valid search is still running (turns a
  stuck spinner into a wrong answer). At most keep a long watchdog as a failure
  fallback.

## Key files

- Backend one-shot: `crates/lychi-core/src/file_search/mod.rs`
  (`fuzzy_path_completions`), `src-tauri/src/commands/filesystem.rs`
  (`start_file_search`, `fuzzy_path_completions`, the `file-search-results` emit).
- Frontend state: `src/lib/stores/completions.svelte.ts`
  (`applyFileSearchBatch`, `searchMode`, `searchDone`, `fileSearchId`).
- Event wiring: `src/lib/events/bridge.svelte.ts`, `src/lib/events/router.ts`.
- "Searching…" render: `src/lib/components/CompletionsList.svelte` (note: this
  component appears to have no live importer — verify it is the actual on-screen
  component before editing; the live search list may render elsewhere).

## First step when picking this up

Run the app with devtools and trace what fires when you type `/x`: which IPC
command is invoked, whether `file-search-results` events arrive, and where
"Searching…" is bound. That resolves the "which path is live" question the static
read could not, and tells you whether to (a) finish wiring the streaming path or
(b) add a `done` signal to the one-shot path.

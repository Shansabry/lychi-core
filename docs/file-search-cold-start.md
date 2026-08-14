# File search: "Searching…" sticks on the first / cold query

**Status:** FIXED (backend terminal-emit guarantee). See "The fix" below.
**Severity was:** UX bug — first-query-after-idle and freshly-drilled-subfolder
stuck on "Searching…" until the next keystroke.

## Symptom

The first `/`-file-search after launch/idle, or drilling into a never-indexed
subfolder, shows **"Searching…"** and never populates even though results exist.
Typing one more character makes results appear. Intermittent.

## Root cause (corrected — the earlier version of this doc was wrong)

The earlier diagnosis claimed the streaming FE path was half-wired and the live
`/`-search ran a one-shot with no `done` signal. **That was wrong** — it was
written from a static read that never opened `+page.svelte`. A source trace
confirmed the STREAMING path IS the live path and is fully wired on both ends
(`searchMode` set true at `+page.svelte:443`, `fileSearchId` bumped `:445`,
`startFileSearch` invoked `:472`; `applyFileSearchBatch` correctly handles
`search_id` staleness + the `done` flag). The one-shot `fuzzy_path_completions`
is only used by `@`-browse.

The real bug is a **dropped terminal-emit on a COLD corpus**, backend-side:

1. On a cold scope, `start_file_search`'s initial `emit_ranked_results`
   (`filesystem.rs`) matches against the tiny/partial corpus walked so far and
   emits `done:false` (or an empty `done:true` if `injected_count` is already
   satisfied by an empty snapshot).
2. When the walk finishes it re-injects the full corpus; the matcher must
   re-match and emit the final `done:true`.
3. That final emit rides the matcher's re-seed subscription
   (`live.rs::reseed_on_corpus_change`), which legitimately EARLY-RETURNS when the
   matcher has no active query at publish time / was rebuilt / isn't stale
   (`live.rs:224,226,229`). On the first cold query the publish→notify can land in
   that window → the final emit is dropped → stuck spinner until the next
   keystroke bumps `fileSearchId` and re-runs against the now-warm corpus.
4. Drilling into a subfolder hits the identical bug — a drilled folder is always a
   cold scope.

**Note on `nucleo::Status.running`:** NOT usable as the done-signal here.
`session.rs::results_for` documents (with measurements) that `running` reflects
injection progress, not query completion — a 162k one-burst injection keeps it
`true` almost permanently, reporting `false`-complete on 22/23 finished searches.
`complete = snap.item_count() >= injected_count` is the honest matching-done
signal and is correct; the bug was never the `complete` derivation, only that a
re-matched cold corpus's terminal batch wasn't guaranteed to be emitted.

## The fix

`start_file_search` (`src-tauri/src/commands/filesystem.rs`) attaches a
DEDICATED, event-based one-shot to the corpus that emits one authoritative batch
for the active `search_id` when the walk completes — independent of the matcher's
query state, so it cannot be dropped like the re-seed notification. Race-critical
ordering: **subscribe first, THEN check `is_complete()`** (a walk finishing
between a check-first and the subscribe would fire its notify before the
subscriber existed). A `fired` atomic makes it exactly-once; a duplicate/stale
batch is harmless (FE ignores a stale `search_id`; a repeat `done:true` just
re-latches `searchDone`). This is the industry pattern (fzf EOF / Telescope
`on_exit` / VS Code promise-settle): bind "done" to something that cannot be
missed — here, a guaranteed terminal emit on corpus completion.

## Deferred polish (not done — separate from the bug)

- **Stalled-spinner delay (~200ms, Algolia `stalledSearchDelay`).** Don't render
  "Searching…" for the first ~150–200ms so warm queries never flash a spinner.
  Frontend-only: a `searchStalled` state gating `+page.svelte:2143,2152` +
  `CompletionsList.svelte:173`. Nice-to-have; the stuck-spinner bug is fixed
  without it.
- **Prewarm drilled/non-home scopes** on drill so the terminal-emit path rarely
  has to wait. Latency optimization on top of the correctness fix.

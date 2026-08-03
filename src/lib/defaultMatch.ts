/**
 * Which completion — if any — Enter runs when the user has not arrow-selected one.
 *
 * # The rule
 *
 * A suggestion may become Enter's default ONLY if it prefix-extends what the
 * user literally typed. This is the browser-omnibox model (Chromium's
 * `allowed_to_be_default_match`), not Spotlight's, and the reason is specific
 * to Lychi: it has literal `run`/shell commands where the typed text IS the
 * thing to execute. "First plausible row" is therefore not a safe default —
 * it silently runs something other than what was typed.
 *
 *   run top   + history `run htop`  → not a prefix → Enter runs `run top`
 *   fir       + app `Firefox`       → prefix       → Enter runs firefox
 *
 * Apps get no special privilege; it is purely the prefix predicate.
 *
 * # Why this lives in its own module
 *
 * It previously lived inside `+page.svelte`, where it could not be unit-tested
 * — and the prefix check was silently lost, leaving `findIndex(first
 * non-fallback row)`. The symptom was immediate once looked for: typing
 * `services` auto-selected the "Session and Startup" app, so Enter launched an
 * app instead of running the command. That is the same failure the rule was
 * written for, recurring because nothing could catch it. Extracted here so the
 * predicate has tests.
 */

import type { CompletionItem } from "$lib/bindings";

/** Icons marking rows that are decoration, not results. */
export const NON_ACTIONABLE_ICONS = new Set(["__separator__", "__info__"]);

/**
 * Escape-hatch rows ("Ask AI" / "Search web"). Always present, never Enter's
 * default: auto-selecting them once meant Enter on a question ran whichever
 * fallback frecency floated up, competing with the input classifier.
 * Mirrors the Rust `CompletionKind::is_fallback`.
 */
export function isFallbackKind(kind: string | null | undefined): boolean {
	return kind === "ask-ai" || kind === "search-web";
}

/**
 * Index of the completion Enter should default to, or -1 for "run what was
 * typed".
 */
export function defaultMatchIndex(results: CompletionItem[], input: string): number {
	const typed = input.trim().toLowerCase();
	if (!typed) return -1;
	return results.findIndex((c) => {
		if (isFallbackKind(c.kind)) return false;
		if (c.icon_path && NON_ACTIONABLE_ICONS.has(c.icon_path)) return false;
		// `run` is the exact command, `fill` the tab-completion text, `label` the
		// display string — checked in that order because that is the precedence
		// the executor uses, so the test matches what would actually run.
		const text = (c.run ?? c.fill ?? c.label).toLowerCase();
		return text.startsWith(typed);
	});
}

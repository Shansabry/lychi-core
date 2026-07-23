/**
 * Window-level keyboard dispatcher (capture phase).
 *
 * Some shortcuts must fire even when DOM focus has left the input — e.g. after a
 * command runs and the result panel is showing. Previously this was an inline
 * `window.addEventListener("keydown", …)` ladder in +page.svelte's onMount. It's
 * pulled out here so the binding→effect mapping is in one place and the page just
 * supplies the effects (which touch page-owned refs/state).
 *
 * The effects stay page-coupled by design (`copyQr` on a component ref, hiding the
 * window to screenshot, reading the current result) — this is a dispatcher, not a
 * store, so it takes a small callback surface rather than owning that state.
 */

import { matchesAction } from "$lib/keybindings";
import { completions } from "$lib/stores/completions.svelte";

export interface WindowKeyEffects {
	/** Whether the current result carries an inline-openable URL. */
	hasInlineUrl: () => boolean;
	/** Whether the current result is a QR/svg (copy_path copies the image). */
	isSvgResult: () => boolean;
	openInlineUrl: () => void;
	copyQr: () => void;
	screenshot: () => void;
}

/**
 * Handle one window-level keydown. Returns true if it consumed the event (the
 * caller has already had `preventDefault()` applied where needed).
 */
export function handleWindowKey(e: KeyboardEvent, fx: WindowKeyEffects): void {
	// Prevent WebKitGTK from stealing tab_back for native focus navigation.
	if (matchesAction(e, "tab_back")) {
		e.preventDefault();
		return;
	}
	// Browse the current result's inline URL (focus-independent — fires from the
	// result panel too).
	if (fx.hasInlineUrl() && matchesAction(e, "open_inline_url")) {
		e.preventDefault();
		fx.openInlineUrl();
		return;
	}
	// Copy a shown QR image with copy_path (Ctrl+Shift+C). Only when a QR result is
	// visible and NOT in file-search mode (there the same binding copies the
	// selected path — no conflict).
	if (!completions.searchMode && fx.isSvgResult() && matchesAction(e, "copy_path")) {
		e.preventDefault();
		fx.copyQr();
		return;
	}
	// Quick screenshot (Ctrl+Shift+P): hide Lychi so it's not in the shot, then
	// capture a region. Fires from anywhere in the window.
	if (matchesAction(e, "screenshot")) {
		e.preventDefault();
		fx.screenshot();
	}
}

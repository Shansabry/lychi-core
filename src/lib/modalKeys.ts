/**
 * Who owns a keystroke while a modal is awaiting a decision.
 *
 * This exists because I-016 was a *distributed* bug: the rule "a confirmation
 * prompt owns the keyboard" was implied in three places and stated in none.
 * `CommandInput` bound the shortcut table to the input element, `AiAnswer` bound
 * approve/reject to `window`, and `ResultPanel` hardcoded `e.key === "Enter"`.
 * Keydown bubbles element → window, so the input always won: Ctrl+Enter matched
 * `web_search`, called preventDefault(), and ran a web search while a
 * confirmation prompt sat on screen advertising that key as "Approve".
 *
 * Both decisions live here as pure functions so they can be tested without a
 * component harness — the absence of one is why the previous render bug shipped.
 */
import { type KeyLike, matchesAction } from "$lib/keybindings";

export type { KeyLike };

/**
 * Should the launcher input's shortcut table handle this key?
 *
 * False whenever a modal is awaiting a yes/no. Standing down wholesale rather
 * than exempting the approve combo: every binding in that table (⌘K,
 * run_inline, the Tab family) is equally wrong to fire while a destructive
 * command awaits a decision, and enumerating the safe ones leaves the same
 * latent bug for whichever binding is added next.
 *
 * Typing is unaffected — this gates the shortcut table, not text entry.
 */
export function inputOwnsKey(decisionPending: boolean): boolean {
	return !decisionPending;
}

/** What a confirmation prompt should do with a keystroke. */
export type ConfirmIntent = "approve" | "reject" | "ignore";

/**
 * Resolve a keystroke against a confirmation prompt.
 *
 * `approve_action` is checked BEFORE plain Enter. `e.key` is "Enter" with or
 * without Ctrl, so a raw-Enter test passes for the Ctrl+Enter combo too —
 * ordering it first would mask the binding and re-create the ambiguity that
 * made the two confirmation surfaces disagree about their own approve key.
 *
 * Plain Enter still approves: it is what the panel has always advertised, and
 * the default binding puts `approve_action` on Ctrl+Enter, so both must work.
 */
export function confirmIntent(e: KeyLike): ConfirmIntent {
	if (matchesAction(e, "approve_action")) return "approve";
	if (matchesAction(e, "reject_action")) return "reject";
	if (e.key === "Enter") return "approve";
	if (e.key === "Escape") return "reject";
	return "ignore";
}

import { beforeEach, describe, expect, it } from "vitest";
import { KEYBINDINGS_DEFAULTS } from "./ipc";
import { loadKeybindings } from "./keybindings";
import { confirmIntent, inputOwnsKey, type KeyLike } from "./modalKeys";

/**
 * I-016 regression cover.
 *
 * Ctrl+Enter on a confirmation prompt ran a web search instead of approving.
 * Two independent faults produced it, so both need pinning separately:
 *
 *   1. `CommandInput` handled the shortcut table even while a modal was up, and
 *      won the race by binding to the input element (keydown bubbles element →
 *      window, and the approval prompt listens on window).
 *   2. `ResultPanel` hardcoded `e.key === "Enter"` and never consulted the
 *      binding table, so it disagreed with the AI prompt about its own key.
 *
 * These live against the pure functions rather than the components: there is no
 * component-render harness in this project, which is precisely why the previous
 * render bug (`each_key_duplicate`) shipped unnoticed.
 */

function key(
	k: string,
	mods: Partial<Record<"ctrl" | "shift" | "alt" | "meta", boolean>> = {},
): KeyLike {
	return {
		key: k,
		code: "",
		ctrlKey: !!mods.ctrl,
		shiftKey: !!mods.shift,
		altKey: !!mods.alt,
		metaKey: !!mods.meta,
	};
}

beforeEach(() => {
	loadKeybindings(KEYBINDINGS_DEFAULTS);
});

describe("inputOwnsKey — fault 1: the input must stand down for a modal", () => {
	it("yields the keyboard while a decision is pending", () => {
		expect(inputOwnsKey(true)).toBe(false);
	});

	it("keeps the keyboard when nothing is pending", () => {
		expect(inputOwnsKey(false)).toBe(true);
	});
});

describe("confirmIntent — fault 2: the prompt must honour the binding table", () => {
	it("approves on Ctrl+Enter — the bug: this ran a web search", () => {
		// The whole issue in one assertion. `web_search` shares this combo, and
		// the input used to consume it first.
		expect(confirmIntent(key("Enter", { ctrl: true }))).toBe("approve");
	});

	it("still approves on plain Enter", () => {
		// The panel has always advertised Enter; the fix must not trade one
		// working key for another.
		expect(confirmIntent(key("Enter"))).toBe("approve");
	});

	it("rejects on Escape", () => {
		expect(confirmIntent(key("Escape"))).toBe("reject");
	});

	it("ignores keys that mean nothing to a prompt", () => {
		expect(confirmIntent(key("k", { ctrl: true }))).toBe("ignore");
		expect(confirmIntent(key("a"))).toBe("ignore");
		expect(confirmIntent(key("ArrowDown"))).toBe("ignore");
	});

	it("follows a rebind instead of the hardcoded key", () => {
		// The actual regression guard. A raw `e.key === "Enter"` check passes
		// every test above by accident — it only fails once approve moves off
		// Enter entirely. Without consulting the binding table this returns
		// "approve" for Enter and "ignore" for Alt+A: both wrong.
		loadKeybindings({ ...KEYBINDINGS_DEFAULTS, approve_action: "Alt+A" });
		expect(confirmIntent(key("a", { alt: true }))).toBe("approve");
	});

	it("prefers the bound combo over the raw-Enter fallback", () => {
		// Ordering guard. `e.key` is "Enter" with or without Ctrl, so checking
		// raw Enter first would shadow the binding and silently reintroduce the
		// ambiguity between approve and web-search.
		loadKeybindings({ ...KEYBINDINGS_DEFAULTS, approve_action: "Enter" });
		expect(confirmIntent(key("Enter"))).toBe("approve");
		loadKeybindings({ ...KEYBINDINGS_DEFAULTS, reject_action: "Enter" });
		expect(confirmIntent(key("Enter"))).toBe("reject");
	});
});

import { describe, expect, it } from "vitest";
import { KEYBINDINGS_DEFAULTS } from "./ipc";
import {
	ACTION_LABELS,
	type ActionId,
	ALL_ACTIONS,
	findConflicts,
	getComboString,
	loadKeybindings,
	matchesAction,
} from "./keybindings";

/**
 * These tests exist because shortcuts had drifted into TWO systems: the
 * configurable table here, and hardcoded `e.ctrlKey && e.key === "Enter"` checks
 * with literal "⌘↵" labels in components. The same action ended up with two
 * definitions and two on-screen spellings.
 *
 * What follows pins the invariants that keep it one system.
 */

/** A KeyboardEvent stand-in — only the fields `matchesAction` reads. */
function key(k: string, mods: Partial<Record<"ctrl" | "shift" | "alt" | "meta", boolean>> = {}) {
	return {
		key: k,
		code: "",
		ctrlKey: !!mods.ctrl,
		shiftKey: !!mods.shift,
		altKey: !!mods.alt,
		metaKey: !!mods.meta,
	} as KeyboardEvent;
}

describe("keybindings — the config is the single source of truth", () => {
	it("every action has a default binding and a label", () => {
		// A missing entry means an action that can't be displayed or rebound —
		// which is how hardcoded shortcuts got introduced in the first place.
		for (const action of ALL_ACTIONS) {
			expect(KEYBINDINGS_DEFAULTS[action], `${action} has no default`).toBeTruthy();
			expect(ACTION_LABELS[action], `${action} has no label`).toBeTruthy();
		}
	});

	it("the Rust defaults and the TS action list agree", () => {
		// Drift here means the backend knows about a binding the frontend can't
		// show, or vice-versa.
		const fromConfig = Object.keys(KEYBINDINGS_DEFAULTS).sort();
		const fromActions = [...ALL_ACTIONS].sort();
		expect(fromConfig).toEqual(fromActions);
	});

	it("defaults contain no unintended conflicts", () => {
		expect(findConflicts(KEYBINDINGS_DEFAULTS)).toEqual([]);
	});
});

describe("keybindings — deliberately shared bindings", () => {
	it("a modal action sharing a key is not reported as a conflict", () => {
		// The approval prompt is modal — while it's up the launcher input isn't
		// listening, so `approve_action` sharing Ctrl+Enter with `web_search` is
		// deliberate. Warning here would train users to ignore the conflict
		// warning that exists for real clashes.
		const shared = findConflicts(KEYBINDINGS_DEFAULTS).flat();
		expect(shared).not.toContain("approve_action");
	});

	it("a genuine clash IS still reported", () => {
		const clashing = { ...KEYBINDINGS_DEFAULTS, toggle_media: KEYBINDINGS_DEFAULTS.toggle_history };
		expect(findConflicts(clashing).length).toBeGreaterThan(0);
	});
});

describe("keybindings — labels are rendered, never written by hand", () => {
	it("uses Linux notation, not the macOS Command glyph", () => {
		// Lychi is Linux-only; "⌘↵" was shipped in the AI answer card.
		for (const action of ALL_ACTIONS) {
			const combo = getComboString(action);
			expect(combo, `${action} renders a macOS glyph`).not.toMatch(/[⌘⌥⇧]/);
		}
	});

	it("a rebind moves the handler AND the label together", () => {
		loadKeybindings({ ...KEYBINDINGS_DEFAULTS, web_search: "Alt+W" });
		expect(getComboString("web_search")).toBe("Alt+W");
		expect(matchesAction(key("w", { alt: true }), "web_search")).toBe(true);
		// The old binding no longer fires — the whole point of one system.
		expect(matchesAction(key("Enter", { ctrl: true }), "web_search")).toBe(false);

		loadKeybindings(KEYBINDINGS_DEFAULTS); // restore for other tests
	});

	it("approve/reject resolve to their configured combos", () => {
		expect(matchesAction(key("Enter", { ctrl: true }), "approve_action")).toBe(true);
		expect(matchesAction(key("Escape"), "reject_action")).toBe(true);
	});
});

describe("keybindings — ActionId coverage", () => {
	it("the actions added for the AI surface are bindable", () => {
		// These were raw key checks in AiAnswer.svelte with no config entry, so
		// they could be neither rebound nor displayed consistently.
		const added: ActionId[] = ["approve_action", "reject_action"];
		for (const a of added) {
			expect(ALL_ACTIONS).toContain(a);
			expect(getComboString(a)).toBeTruthy();
		}
	});
});

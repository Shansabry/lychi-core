/**
 * Configurable keyboard shortcut resolution.
 *
 * Module-level Maps for O(1) lookup. No Svelte store needed —
 * call `loadKeybindings()` once at startup from the preload cache.
 */
import type { KeybindingsConfig } from "$lib/ipc";
import { KEYBINDINGS_DEFAULTS } from "$lib/ipc";

export type ActionId =
	| "toggle_history"
	| "toggle_notes"
	| "toggle_media"
	| "toggle_settings"
	| "open_inline_url"
	| "submit"
	| "dismiss"
	| "tab_complete"
	| "tab_back"
	| "switch_scope"
	| "web_search"
	| "run_inline"
	| "copy_path"
	| "screenshot"
	| "action_panel";

export const ACTION_LABELS: Record<ActionId, string> = {
	toggle_history: "Toggle history",
	toggle_notes: "Toggle notes",
	toggle_media: "Toggle media",
	toggle_settings: "Toggle settings",
	open_inline_url: "Open result in browser",
	submit: "Submit command",
	dismiss: "Dismiss / hide",
	tab_complete: "Accept completion",
	tab_back: "Go back / up folder",
	switch_scope: "Switch result scope",
	web_search: "Search web",
	run_inline: "Run command inline",
	copy_path: "Copy result path",
	screenshot: "Quick screenshot",
	action_panel: "Show actions for result",
};

export const ALL_ACTIONS: ActionId[] = Object.keys(ACTION_LABELS) as ActionId[];

interface ParsedBinding {
	key: string;
	ctrl: boolean;
	shift: boolean;
	alt: boolean;
	meta: boolean;
}

const bindings = new Map<ActionId, ParsedBinding>();
const reverseMap = new Map<string, ActionId>();

function parseCombo(combo: string): ParsedBinding {
	const parts = combo.split("+");
	const key = parts[parts.length - 1].toLowerCase();
	return {
		key,
		ctrl: parts.some((p) => p.toLowerCase() === "ctrl"),
		shift: parts.some((p) => p.toLowerCase() === "shift"),
		alt: parts.some((p) => p.toLowerCase() === "alt"),
		meta: parts.some((p) => p.toLowerCase() === "super" || p.toLowerCase() === "meta"),
	};
}

// Map e.code → logical key name for when WebKitGTK returns "Unidentified" for e.key
const CODE_TO_KEY: Record<string, string> = {
	Tab: "tab",
	Space: "space",
	Enter: "enter",
	Escape: "escape",
	Backspace: "backspace",
	Delete: "delete",
	ArrowUp: "arrowup",
	ArrowDown: "arrowdown",
	ArrowLeft: "arrowleft",
	ArrowRight: "arrowright",
};

export function normalizeKey(key: string, code?: string): string {
	if (key === "Unidentified" && code) {
		return CODE_TO_KEY[code] ?? code.toLowerCase();
	}
	if (key === " ") return "space";
	return key.toLowerCase();
}

function fingerprint(
	ctrl: boolean,
	shift: boolean,
	alt: boolean,
	meta: boolean,
	key: string,
): string {
	return `${ctrl ? 1 : 0}|${shift ? 1 : 0}|${alt ? 1 : 0}|${meta ? 1 : 0}|${key}`;
}

function eventFp(e: KeyboardEvent): string {
	return fingerprint(e.ctrlKey, e.shiftKey, e.altKey, e.metaKey, normalizeKey(e.key, e.code));
}

function bindingFp(b: ParsedBinding): string {
	return fingerprint(b.ctrl, b.shift, b.alt, b.meta, b.key);
}

/** Parse a KeybindingsConfig and populate the lookup maps. */
export function loadKeybindings(config: KeybindingsConfig) {
	bindings.clear();
	reverseMap.clear();
	for (const [action, combo] of Object.entries(config)) {
		const parsed = parseCombo(combo);
		bindings.set(action as ActionId, parsed);
		reverseMap.set(bindingFp(parsed), action as ActionId);
	}
}

/** Check if a KeyboardEvent matches a specific action. */
export function matchesAction(e: KeyboardEvent, action: ActionId): boolean {
	const b = bindings.get(action);
	if (!b) return false;
	return eventFp(e) === bindingFp(b);
}

/** Get which action (if any) a KeyboardEvent maps to. */
export function getAction(e: KeyboardEvent): ActionId | null {
	return reverseMap.get(eventFp(e)) ?? null;
}

/** Get the display string for an action (e.g. "Ctrl+1"). */
export function getComboString(action: ActionId): string {
	const b = bindings.get(action);
	if (!b) return "";
	const parts: string[] = [];
	if (b.ctrl) parts.push("Ctrl");
	if (b.shift) parts.push("Shift");
	if (b.alt) parts.push("Alt");
	if (b.meta) parts.push("Super");
	parts.push(b.key.length === 1 ? b.key.toUpperCase() : capitalize(b.key));
	return parts.join("+");
}

/** Return the default keybindings config. */
export function getDefaults(): KeybindingsConfig {
	return { ...KEYBINDINGS_DEFAULTS };
}

/** Find conflicting action pairs that share the same key combo. */
export function findConflicts(config: KeybindingsConfig): [ActionId, ActionId][] {
	const seen = new Map<string, ActionId>();
	const conflicts: [ActionId, ActionId][] = [];
	for (const [action, combo] of Object.entries(config)) {
		const fp = bindingFp(parseCombo(combo));
		const existing = seen.get(fp);
		if (existing) {
			conflicts.push([existing, action as ActionId]);
		} else {
			seen.set(fp, action as ActionId);
		}
	}
	return conflicts;
}

/** Build a combo string from a KeyboardEvent (for recording UI). */
export function comboFromEvent(e: KeyboardEvent): string | null {
	// Ignore bare modifier keys
	if (["Control", "Shift", "Alt", "Meta"].includes(e.key)) return null;

	const parts: string[] = [];
	if (e.ctrlKey) parts.push("Ctrl");
	if (e.shiftKey) parts.push("Shift");
	if (e.altKey) parts.push("Alt");
	if (e.metaKey) parts.push("Super");

	let keyName = e.key;
	if (keyName === " ") keyName = "Space";
	else if (keyName.length === 1) keyName = keyName.toUpperCase();
	parts.push(keyName);

	return parts.join("+");
}

function capitalize(s: string): string {
	return s.charAt(0).toUpperCase() + s.slice(1);
}

// Initialize with defaults so shortcuts work even before preload resolves
loadKeybindings(KEYBINDINGS_DEFAULTS);

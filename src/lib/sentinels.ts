/**
 * The `__sentinel__` strings the backend puts in `icon_path` / `run`, in one
 * place.
 *
 * # Why this file exists
 *
 * Everything else crosses IPC as a tauri-specta generated type. These do not:
 * they are magic strings the backend writes and the frontend matches by hand,
 * across four files, with no compile-time link between the two ends. A backend
 * rename therefore breaks the UI **silently** — the row simply stops rendering
 * as a folder, or a panel stops opening, with nothing to fail.
 *
 * Collecting them does not make the channel typed (the Rust side still emits
 * bare strings), but it does mean:
 *
 *   - one definition per sentinel instead of ~23 scattered literals,
 *   - a typo is a TypeScript error rather than a silently-false comparison,
 *   - `BUILTIN_ICONS` replaces the 400-character `hasCustomIcon` expression
 *     that enumerated every icon sentinel by hand and had to be edited by hand
 *     each time one was added,
 *   - and `src-tauri/src/state.rs` has a test asserting the Rust side still
 *     emits exactly these, so the two cannot drift apart unnoticed.
 */

/** `icon_path` values that select a built-in glyph rather than a file path. */
export const ICON = {
	folder: "__folder__",
	none: "__none__",
	web: "__web__",
	history: "__history__",
	separator: "__separator__",
	warning: "__warning__",
	context: "__context__",
	contextStale: "__context_stale__",
	terminal: "__terminal__",
	info: "__info__",
	clipboardImage: "__clipboard_image__",
	aiChat: "__ai_chat__",
	pinned: "__pinned__",
} as const;

export type IconSentinel = (typeof ICON)[keyof typeof ICON];

/**
 * Every built-in `icon_path`. An `icon_path` outside this set is a real
 * filesystem path to an image.
 *
 * Derived from `ICON` rather than written out again — adding a sentinel above
 * is enough, which is the whole point.
 */
export const BUILTIN_ICONS: ReadonlySet<string> = new Set(Object.values(ICON));

/** Is this `icon_path` a real image path rather than a built-in marker? */
export function isCustomIcon(iconPath: string | null | undefined): boolean {
	return !!iconPath && !BUILTIN_ICONS.has(iconPath);
}

/** `run` values that open a panel instead of executing a command. */
export const PANEL = {
	media: "__media_panel__",
	notes: "__notes_panel__",
	timer: "__timer_panel__",
	reminders: "__reminders_panel__",
	snippets: "__snippets_panel__",
} as const;

export type PanelSentinel = (typeof PANEL)[keyof typeof PANEL];

export const PANELS: ReadonlySet<string> = new Set(Object.values(PANEL));

/**
 * Sentinels carrying a payload after the colon (`__chat__:<id>`), so they are
 * matched with `startsWith` rather than equality.
 */
export const PREFIX = {
	browsePanel: "__browse_panel__:",
	notesLimit: "__notes_limit__:",
	chat: "__chat__:",
} as const;

/** The payload after a prefix sentinel, or `null` if it does not match. */
export function payloadAfter(value: string | null | undefined, prefix: string): string | null {
	if (!value?.startsWith(prefix)) return null;
	return value.slice(prefix.length);
}

/** Non-icon markers used in other fields. */
export const MARKER = {
	/** A warm-up completions request; not a real query. */
	warmup: "__warmup__",
	/** "use the default" in settings pickers. */
	default: "__default__",
	/** "user-supplied value" in settings pickers. */
	custom: "__custom__",
} as const;

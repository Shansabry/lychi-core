/**
 * Pure parsing for the launcher's two input modes — `/` file-search and `@`
 * file-reference. These are the off-by-one-prone string splices that were
 * copy-pasted across ~6 sites in `+page.svelte` (three `/`-parses, several
 * `@`-token boundary computations) plus `CommandInput.svelte`'s highlight
 * overlay. Collecting them here means the trailing-slash, embedded-space, and
 * email-guard edges are decided ONCE and unit-tested, instead of living inline
 * in markup (invisible to Vitest) and drifting between callers.
 *
 * Everything here is a pure function of its inputs — no store access, no DOM,
 * no side effects — so each edge is testable without a backend or a browser.
 */

/** The parsed shape of a `/`-search input. */
export interface SearchParse {
	/** The folder path typed after the leading `/` and before the last `/`
	 *  (e.g. `Documents/Reports`), or `""` when the query is at the top level. */
	folder: string;
	/** The search term — the text after the last `/` (or the whole thing when
	 *  there is no folder). */
	term: string;
	/** True when the input contains at least one `/` after the leading one, i.e.
	 *  a folder was specified. Mirrors the old `lastSlash >= 0` branch. */
	hasFolder: boolean;
	/** True when the TERM contains a space. A folder path may contain spaces, but
	 *  a space in the term means this isn't a search input (the caller bails and
	 *  treats the text as a normal query). Mirrors the old
	 *  `!searchTermCandidate.includes(" ")` guard. */
	termHasSpace: boolean;
}

/**
 * Parse a `/`-search input. `raw` is the text AFTER the leading `/` (callers
 * pass `value.slice(1)`), matching the three inline parses this replaces.
 *
 * `/Documents/Reports/q1` → `{ folder: "Documents/Reports", term: "q1",
 * hasFolder: true, termHasSpace: false }`.
 */
export function parseSearchInput(raw: string): SearchParse {
	const lastSlash = raw.lastIndexOf("/");
	const term = lastSlash >= 0 ? raw.slice(lastSlash + 1) : raw;
	const folder = lastSlash >= 0 ? raw.slice(0, lastSlash) : "";
	return {
		folder,
		term,
		hasFolder: lastSlash >= 0,
		termHasSpace: term.includes(" "),
	};
}

/**
 * Compose the absolute search scope from a base mount path and a parsed input.
 * `folder` is appended to `base` only when present, so a top-level query scopes
 * to the base itself. Mirrors `folderPart ? \`${base}/${folder}\` : base`.
 */
export function searchScope(base: string, parsed: SearchParse): string {
	return parsed.folder ? `${base}/${parsed.folder}` : base;
}

/**
 * The PARENT of a `/`-search input, for the "go up a level" (Shift+Tab)
 * gesture. Takes `raw` (text after the leading `/`) and returns the full new
 * input value INCLUDING the leading `/`. Strips a trailing slash first so
 * `/a/b/` goes to `/a/`, and stops at the root (`/`).
 *
 * `a/b/c`  → `/a/b/`
 * `a/b/`   → `/a/`
 * `a`      → `/`
 */
export function parentSearchInput(raw: string): string {
	const trimmed = raw.endsWith("/") ? raw.slice(0, -1) : raw;
	const lastSlash = trimmed.lastIndexOf("/");
	return lastSlash > 0 ? `/${trimmed.slice(0, lastSlash + 1)}` : "/";
}

/** The three pieces of an input split around its active `@`-reference token. */
export interface AtToken {
	/** Everything before the `@`. */
	before: string;
	/** The `@…` token itself, up to the first space after the `@` (or end). */
	atPart: string;
	/** Everything from that space onward (including the leading space), or `""`
	 *  when the token runs to the end of the input. */
	after: string;
}

/**
 * Split `value` around the `@`-reference token that starts at `atStart` (the
 * index of the `@`). The token ends at the first space AFTER the `@` (the
 * `indexOf(" ", 1)` — offset 1 so the `@` itself is skipped), or the end of the
 * string. This is the boundary the completion splices and the highlight overlay
 * each used to recompute independently.
 */
export function atToken(value: string, atStart: number): AtToken {
	const before = value.slice(0, atStart);
	const afterAt = value.slice(atStart);
	const spaceIdx = afterAt.indexOf(" ", 1);
	const atPart = spaceIdx === -1 ? afterAt : afterAt.slice(0, spaceIdx);
	const after = spaceIdx === -1 ? "" : afterAt.slice(spaceIdx);
	return { before, atPart, after };
}

/**
 * Replace the active `@`-token's payload with `path`, keeping the surrounding
 * text. Returns the new input value. `path` is inserted as `@<path>` (no
 * trailing space — a folder keeps browsing, a file is a complete reference the
 * caller finalises). Mirrors `\`${before}@${path}${after}\``.
 */
export function spliceAtToken(value: string, atStart: number, path: string): string {
	const { before, after } = atToken(value, atStart);
	return `${before}@${path}${after}`;
}

/**
 * The `@`-reference partial currently under the cursor — the text after the
 * `@`, up to the token's end. Used to drive completion fetches and the ghost.
 */
export function atPartial(value: string, atStart: number): string {
	const { atPart } = atToken(value, atStart);
	return atPart.slice(1); // drop the leading `@`
}

/**
 * The PARENT of an `@`-reference partial, for the "go up a level" gesture in
 * `@`-browse. Given the current partial (text after `@`), returns the parent
 * partial (with trailing slash) or `""` at the top. Mirrors the Shift+Tab
 * `@`-branch: strip trailing slash, cut at the last slash.
 *
 * `Documents/Reports/`  → `Documents/`
 * `Documents/`          → `""`
 */
export function parentAtPartial(partial: string): string {
	const trimmed = partial.endsWith("/") ? partial.slice(0, -1) : partial;
	const lastSlash = trimmed.lastIndexOf("/");
	return lastSlash > 0 ? trimmed.slice(0, lastSlash + 1) : "";
}

/**
 * Whether the `@` at `atIdx` is a real file-reference trigger rather than the
 * `@` in an email address. An email has non-space text immediately before the
 * `@` (`user@host`); a reference has a space or start-of-input before it.
 * Mirrors the `beforeAt.length > 0 && !beforeAt.endsWith(" ")` guard.
 */
export function isEmailAt(value: string, atIdx: number): boolean {
	const beforeAt = value.slice(0, atIdx);
	return beforeAt.length > 0 && !beforeAt.endsWith(" ");
}

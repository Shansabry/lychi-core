/**
 * The output decider — the single place that answers "how does this result
 * render, and what can you do with it?"
 *
 * # Why this exists
 *
 * Both halves of that question were previously answered in several places at
 * once, by matching on `output_type` wherever the answer was needed:
 *
 * - `ResultPanel.svelte` grew an `{#if isTerminal} … {:else if isSvg} … {:else}`
 *   chain, one branch per type, so adding a type meant editing a component.
 * - `+page.svelte` separately hardcoded `isSvgResult: () => output_type === "svg"`
 *   to decide whether a copy shortcut applied.
 *
 * Two sites reading the same field to make related decisions is the shape that
 * has caused real bugs in this codebase: when several places each derive a
 * verdict independently, they drift, and nothing owns the truth. The launcher
 * had exactly this with window visibility (three subsystems polling GTK), and
 * the fix was the same — one owner, everything else reads it.
 *
 * # What it gives you
 *
 * `resolveOutput(result)` returns which renderer to use, plus the actions that
 * result supports. The actions half is the point: the ⌘K panel was a fixed menu
 * that could not know a terminal result should offer "continue in terminal" or
 * that a markdown result should offer "copy as markdown". Deriving actions from
 * the output type here means the panel becomes adaptive without any surface
 * needing to know the type system.
 *
 * Pure and DOM-free, so the mapping is unit-testable without mounting a
 * component — which is what makes it safe to keep adding variants.
 */

import type { CommandResult } from "$lib/ipc";

/** Which component renders the payload. One per output shape. */
export type Renderer =
	| "none"
	| "status"
	| "text"
	| "markdown"
	| "terminal"
	| "svg"
	| "weather"
	| "rows";

/**
 * A thing the user can do with a result.
 *
 * `id` is matched by the host surface, which owns the implementation — this
 * module decides *what is offered*, never *how it runs*. Anything privileged
 * (re-running a command, opening a path) must still go through the normal
 * execution path and its rules-engine gate; an action here is a menu entry, not
 * an escape hatch around validation.
 */
export interface OutputAction {
	id: string;
	label: string;
	/** Keybinding action id, when this action also has a global shortcut. */
	binding?: string;
	/** True for the action Enter should take, if any. */
	primary?: boolean;
}

export interface ResolvedOutput {
	renderer: Renderer;
	/** The raw payload for the renderer. Typed per-variant as shapes land. */
	body: string;
	actions: OutputAction[];
}

/**
 * Does this result have something to show the user?
 *
 * Callers used to ask `success && !output` and hide the window when true. That
 * conflates "produced no payload" with "produced nothing in the `output`
 * string", which stopped being the same thing the moment a result could carry
 * structure instead: a `Rows` result sets `sections` and leaves `output` empty,
 * so a perfectly good list of 55 services looked like a bare success and the
 * launcher dismissed itself before rendering it.
 *
 * Asking the decider keeps that judgement in one place, so the next variant
 * added does not have to remember to update two hide checks in `+page.svelte`.
 */
export function hasVisibleOutput(result: CommandResult): boolean {
	return resolveOutput(result).renderer !== "none";
}

/** Actions every non-empty result supports. */
function baseActions(result: CommandResult): OutputAction[] {
	const out: OutputAction[] = [];
	if (result.output) {
		out.push({ id: "copy_output", label: "Copy output", binding: "copy_path" });
	}
	if (result.open_url) {
		out.push({ id: "open_url", label: "Open link", binding: "open_inline_url" });
	}
	return out;
}

/**
 * Resolve a result to its renderer and action set.
 *
 * The `output_type` default of `"status"` matches the previous inline default in
 * `ResultPanel`; keeping it here means a result with no type still has exactly
 * one interpretation rather than one per call site.
 */
export function resolveOutput(result: CommandResult): ResolvedOutput {
	// Structured rows win over `output_type`: a handler that produced rows has
	// said what shape its result is, and the legacy string field is irrelevant.
	// Row actions come from the rows themselves, declared by the handler that
	// enumerated them — the frontend never invents what a row can do.
	if (result.sections) {
		return { renderer: "rows", body: "", actions: baseActions(result) };
	}
	const type = result.output_type ?? "status";
	const body = result.output ?? "";
	const actions = baseActions(result);

	switch (type) {
		case "terminal":
			return {
				renderer: "terminal",
				body,
				actions: [
					// The escalation path out of a captured, non-interactive run.
					// Inline output is stdin-less and capped, so without this the
					// result is a dead end: you can read it and do nothing else.
					{ id: "continue_in_terminal", label: "Continue in terminal", primary: true },
					{ id: "rerun", label: "Run again" },
					{ id: "copy_command", label: "Copy command" },
					...actions,
				],
			};

		case "svg":
			return {
				renderer: "svg",
				body,
				// Replaces the hardcoded `isSvgResult` check in +page.svelte: the
				// capability now travels with the result instead of being
				// re-derived by whoever needs it.
				actions: [
					{ id: "copy_image", label: "Copy image", binding: "copy_path", primary: true },
					...actions,
				],
			};

		case "markdown":
			return {
				renderer: "markdown",
				body,
				actions: [{ id: "copy_markdown", label: "Copy as markdown" }, ...actions],
			};

		case "weather":
			return { renderer: "weather", body, actions };

		case "text":
			return { renderer: "text", body, actions };

		default:
			return { renderer: body ? "status" : "none", body, actions };
	}
}

/**
 * The surface state machine — the single source of truth for "what is visible".
 *
 * The launcher's central invariant is: **exactly one main surface is on screen at
 * a time, with at most one overlay panel above it, and an AI answer that may exist
 * off-screen (parked) but never overlapping.** Historically this invariant was
 * enforced by ~20 hand-rolled sites in +page.svelte each juggling 7 booleans
 * (settingsOpen, notesOpen, aiActive, …) — which is exactly why an AI answer and
 * the Settings panel could render on top of each other: nothing expressed the
 * invariant in one place.
 *
 * This module makes the bad state UNREPRESENTABLE. Visibility is DERIVED from two
 * single-value enums (`surface`, `panel`) plus one orthogonal `aiExists` flag —
 * never stored as independent booleans. "AI visible" and "Settings visible" cannot
 * both be true because each is computed from one enum that holds exactly one value.
 *
 * It is intentionally PURE (no Svelte runes) so the transitions + predicates are
 * unit-testable with the existing Vitest setup (mirrors submit-router.ts). The
 * reactive wrapper lives in `stores/ui.svelte.ts`.
 */

/** Which overlay panel is open. `"none"` = the surface shows through. The named
 *  panels match `submit-router`'s `PanelName` so routing needs no translation. */
export type Panel = "none" | "history" | "chat-history" | "notes" | "media" | "settings";

/** What occupies the main stage when no panel overlays it. */
export type Surface = "results" | "ai" | "plan";

/** The complete visibility state. One value each — overlap is unrepresentable. */
export interface UiSnapshot {
	/** The main-stage occupant (shown only when `panel === "none"`). */
	surface: Surface;
	/** The overlay panel; `"none"` means the surface is visible. */
	panel: Panel;
	/** An AI answer EXISTS in the chat session (may be on-stage or parked). */
	aiExists: boolean;
}

// --- Predicates (what the template reads; all derived, never stored) ---

/** A panel overlays the surface. */
export const anyPanelOpen = (s: UiSnapshot): boolean => s.panel !== "none";

/** The AI answer is the visible surface. */
export const showAi = (s: UiSnapshot): boolean => s.panel === "none" && s.surface === "ai";

/** The completions/result surface is visible. */
export const showResults = (s: UiSnapshot): boolean =>
	s.panel === "none" && s.surface === "results";

/** The agent-plan surface is visible. */
export const showPlan = (s: UiSnapshot): boolean => s.panel === "none" && s.surface === "plan";

/**
 * An AI answer exists but is NOT the visible surface — i.e. it's parked behind a
 * panel or behind the launcher (Esc). Drives the status-bar recall icon. This is
 * the one genuinely orthogonal axis: you can be on any panel/launcher with an
 * answer waiting off-stage.
 */
export const aiParked = (s: UiSnapshot): boolean => s.aiExists && !showAi(s);

/** Whether a specific panel is the open one. */
export const panelVisible = (s: UiSnapshot, p: Panel): boolean => s.panel === p;

// --- Transitions (pure: old snapshot → new snapshot) ---

/** Open (or switch to) a panel. The surface is preserved underneath — if it was
 *  the AI answer, it becomes parked (derives via `aiParked`), never destroyed. */
export function openPanel(s: UiSnapshot, panel: Panel): UiSnapshot {
	return { ...s, panel };
}

/** Close the current panel, returning to the AI answer if one exists (it was
 *  parked), otherwise the results surface. */
export function closePanel(s: UiSnapshot): UiSnapshot {
	return { ...s, panel: "none", surface: s.aiExists ? "ai" : "results" };
}

/** Toggle a panel: open it if closed/other, close it if it's the current one. */
export function togglePanel(s: UiSnapshot, panel: Panel): UiSnapshot {
	return s.panel === panel ? closePanel(s) : openPanel(s, panel);
}

/** A new/continued AI answer becomes the active surface. Closes any panel — you
 *  cannot be "on AI" and "on Settings" at once. Marks an answer as existing. */
export function showAiSurface(s: UiSnapshot): UiSnapshot {
	return { ...s, surface: "ai", panel: "none", aiExists: true };
}

/** Park the on-screen AI answer (Esc from AI): return to the launcher but KEEP the
 *  answer (`aiExists` stays true → `aiParked` derives true, recallable). No-op if
 *  the AI answer isn't the current surface. */
export function parkAi(s: UiSnapshot): UiSnapshot {
	return s.surface === "ai" ? { ...s, surface: "results" } : s;
}

/** Restore a parked AI answer to the surface (status-bar recall icon). */
export function restoreAi(s: UiSnapshot): UiSnapshot {
	return { ...s, surface: "ai", panel: "none", aiExists: true };
}

/** Clear the AI answer entirely (resetChat / new query). The answer is gone; if it
 *  was the surface, fall back to results. */
export function clearAi(s: UiSnapshot): UiSnapshot {
	return { ...s, aiExists: false, surface: s.surface === "ai" ? "results" : s.surface };
}

/** Show the agent-plan surface (closes any panel). */
export function showPlanSurface(s: UiSnapshot): UiSnapshot {
	return { ...s, surface: "plan", panel: "none" };
}

/** Force the plain launcher/results surface: close any panel, drop back to
 *  results. Used when typing a fresh query (does NOT restore a parked answer —
 *  the caller clears the answer separately). */
export function showLauncher(s: UiSnapshot): UiSnapshot {
	return { ...s, surface: "results", panel: "none" };
}

/** Full reset on summon: a pristine, empty launcher with nothing parked. */
export function reset(): UiSnapshot {
	return { surface: "results", panel: "none", aiExists: false };
}

/** The initial state (fresh launcher). */
export const INITIAL: UiSnapshot = { surface: "results", panel: "none", aiExists: false };

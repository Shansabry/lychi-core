import type { HotkeyStatus } from "$lib/ipc";

/// What the first-run banner should say.
///
/// - `welcome` — the hotkey is fine; point at the Guide.
/// - `confirm` — a grab succeeded but is unproven; ask the user to press it.
/// - `broken`  — it cannot work here; offer a rebind and `lychi --toggle`.
export type BannerMode = "welcome" | "confirm" | "broken";

/// Pick the first-run banner mode from the backend's hotkey verdict.
///
/// Pure and separate from the component so the rule can be tested. The backend
/// decides *whether* the hotkey is trustworthy (`lychi_core::hotkey`); this only
/// decides which of three things to say about it, and must never re-derive that
/// judgement from `session_type` — doing exactly that is what let the X11 case
/// go unmentioned before.
///
/// `null` means the status call failed. That is not a reason to greet nobody, so
/// it falls back to `welcome`: the Guide pointer is useful regardless, and
/// claiming a hotkey problem we did not observe would be worse.
export function bannerMode(status: HotkeyStatus | null): BannerMode {
	if (!status) return "welcome";
	if (status.reliable) return "welcome";
	return status.needs_confirmation ? "confirm" : "broken";
}

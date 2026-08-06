import { describe, expect, it } from "vitest";
import type { HotkeyStatus } from "$lib/ipc";
import { bannerMode } from "$lib/onboarding";

function status(over: Partial<HotkeyStatus> = {}): HotkeyStatus {
	return {
		registered: true,
		session_type: "x11",
		desktop: "KDE",
		reliable: false,
		needs_confirmation: false,
		explanation: "",
		...over,
	};
}

describe("first-run banner mode", () => {
	it("greets every first run, even when the hotkey is fine", () => {
		// The H3 gap: a working hotkey previously marked onboarding complete
		// having shown the user nothing at all.
		expect(bannerMode(status({ reliable: true }))).toBe("welcome");
	});

	it("asks the user to press an unproven grab rather than warning them", () => {
		expect(bannerMode(status({ needs_confirmation: true }))).toBe("confirm");
	});

	it("reports a hotkey that cannot work", () => {
		expect(bannerMode(status({ reliable: false, needs_confirmation: false }))).toBe("broken");
	});

	it("never asks for confirmation of a hotkey that already works", () => {
		// `reliable` wins over a stale needs_confirmation: asking the user to
		// test something we know works is a waste of their attention.
		expect(bannerMode(status({ reliable: true, needs_confirmation: true }))).toBe("welcome");
	});

	it("falls back to the welcome when status is unavailable", () => {
		// Claiming a hotkey problem we did not observe would be worse than
		// showing the Guide pointer.
		expect(bannerMode(null)).toBe("welcome");
	});

	it("does not re-derive the verdict from session type", () => {
		// The bug this whole area had: judging the hotkey by whether the session
		// is Wayland instead of by what the backend actually determined.
		for (const session_type of ["x11", "wayland"]) {
			expect(bannerMode(status({ session_type, reliable: true }))).toBe("welcome");
			expect(bannerMode(status({ session_type, needs_confirmation: true }))).toBe("confirm");
		}
	});
});

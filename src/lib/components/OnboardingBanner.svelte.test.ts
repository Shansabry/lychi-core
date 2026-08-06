// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/svelte";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { BannerMode } from "$lib/onboarding";
import OnboardingBanner from "./OnboardingBanner.svelte";

/**
 * The first-run banner is the only thing a new user is shown, and the failure
 * that matters is it rendering nothing — which is exactly what the previous
 * inline version did on a working hotkey, silently.
 */
afterEach(cleanup);

function mount(mode: BannerMode, over: Record<string, unknown> = {}) {
	const oncopy = vi.fn();
	const onopensettings = vi.fn();
	const ondismiss = vi.fn();
	const { container } = render(OnboardingBanner, {
		props: { mode, text: "the hotkey is taken", oncopy, onopensettings, ondismiss, ...over },
	});
	return { oncopy, onopensettings, ondismiss, container };
}

describe("OnboardingBanner", () => {
	it("always renders something to read and a way out", () => {
		for (const mode of ["welcome", "confirm", "broken"] as BannerMode[]) {
			cleanup();
			const { container } = mount(mode);
			// Non-empty prose, not just buttons — the failure being guarded is a
			// banner that appears but says nothing.
			const text = container.querySelector(".hotkey-banner-text");
			expect(text?.textContent?.trim().length, `${mode} has no text`).toBeGreaterThan(20);
			expect(screen.getByRole("button", { name: "Got it" })).toBeTruthy();
		}
	});

	it("points a working install at the Guide", () => {
		const { onopensettings } = mount("welcome");
		screen.getByRole("button", { name: "Open Guide" }).click();
		expect(onopensettings).toHaveBeenCalledWith("guide");
	});

	it("does not nag a working install about its hotkey", () => {
		mount("welcome");
		expect(screen.queryByRole("button", { name: "Change hotkey" })).toBeNull();
		expect(screen.queryByRole("button", { name: /Copy command/ })).toBeNull();
	});

	it("offers a rebind when the hotkey needs attention", () => {
		for (const mode of ["confirm", "broken"] as BannerMode[]) {
			cleanup();
			const { onopensettings } = mount(mode);
			screen.getByRole("button", { name: "Change hotkey" }).click();
			expect(onopensettings).toHaveBeenCalledWith("general");
		}
	});

	it("shows the backend's explanation verbatim when broken", () => {
		// The wording must come from the one decider, not be restated here.
		mount("broken", { text: "the combination is already bound to something else" });
		expect(screen.getByText(/already bound to something else/)).toBeTruthy();
	});

	it("reflects the copied state on the copy button", () => {
		mount("broken", { copied: true });
		expect(screen.getByRole("button", { name: "Copied" })).toBeTruthy();
	});

	it("dismisses from every mode", () => {
		for (const mode of ["welcome", "confirm", "broken"] as BannerMode[]) {
			cleanup();
			const { ondismiss } = mount(mode);
			screen.getByRole("button", { name: "Got it" }).click();
			expect(ondismiss).toHaveBeenCalled();
		}
	});
});

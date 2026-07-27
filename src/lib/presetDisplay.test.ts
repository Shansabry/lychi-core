import { describe, expect, it } from "vitest";
import { PRESET_ATTACH_THRESHOLD, presetDisplay, renderPreset } from "./submit-router";

const big = "word ".repeat(200); // ~1000 chars, well over the threshold
const small = "hello world";

describe("presetDisplay — instruction + attachment split", () => {
	it("folds a large {input} payload into an attachment chip", () => {
		const d = presetDisplay("Summarize the following: {input}", big);
		expect(d.instruction).toBe("Summarize the following: …");
		expect(d.attachment).toBeTruthy();
		expect(d.attachment?.body).toBe(big.trim());
		expect(d.attachment?.label).toMatch(/^Selected text · /);
	});

	it("reads naturally when {input} is mid-template", () => {
		const d = presetDisplay("translate {input} to spanish", big);
		expect(d.instruction).toBe("translate … to spanish");
		expect(d.attachment).toBeTruthy();
	});

	it("keeps a small input inline (no attachment)", () => {
		const d = presetDisplay("Summarize the following: {input}", small);
		expect(d.attachment).toBeUndefined();
		expect(d.instruction).toBe(renderPreset("Summarize the following: {input}", small));
	});

	it("does not fold a template without an {input} slot", () => {
		const d = presetDisplay("Give me a random fact", big);
		expect(d.attachment).toBeUndefined();
	});

	it("formats the char-count label compactly", () => {
		expect(presetDisplay("{input}", "x".repeat(812)).attachment?.label).toBe("Selected text · 812");
		expect(presetDisplay("{input}", "x".repeat(1200)).attachment?.label).toBe(
			"Selected text · 1.2k",
		);
		expect(presetDisplay("{input}", "x".repeat(34_000)).attachment?.label).toBe(
			"Selected text · 34k",
		);
	});

	it("threshold boundary: exactly at threshold folds", () => {
		const atCap = "x".repeat(PRESET_ATTACH_THRESHOLD);
		expect(presetDisplay("{input}", atCap).attachment).toBeTruthy();
		const under = "x".repeat(PRESET_ATTACH_THRESHOLD - 1);
		expect(presetDisplay("{input}", under).attachment).toBeUndefined();
	});
});

import { describe, expect, it } from "vitest";
import { chat } from "$lib/stores/chat.svelte";
import { ui } from "$lib/stores/ui.svelte";
import { route } from "./router";

/**
 * The router is the pure event seam: feeding it a payload should mutate the
 * stores, with no Tauri/DOM involved. These assert the wiring (the deeper
 * behaviour is covered by the store tests + ui-machine tests).
 */
describe("event router — store delegation", () => {
	it("aiLoadState toggles ui.aiLoading", () => {
		route.aiLoadState("loading");
		expect(ui.aiLoading).toBe(true);
		route.aiLoadState("done");
		expect(ui.aiLoading).toBe(false);
	});

	it("agentEvent applies a text delta to the current chat run", () => {
		// Align the chat generation so the event isn't dropped as stale.
		const gen = chat.gen;
		chat.text = "";
		route.agentEvent({ gen, kind: "text", text: "hello", step: null });
		expect(chat.text).toBe("hello");
	});

	it("agentEvent drops events from a superseded run (stale gen)", () => {
		chat.text = "kept";
		route.agentEvent({ gen: chat.gen - 1, kind: "text", text: "X", step: null });
		expect(chat.text).toBe("kept");
	});
});

import { beforeEach, describe, expect, it } from "vitest";
import type { AgentEventDto } from "$lib/ipc";
import { chat } from "./chat.svelte";

/**
 * D3: `applyEvent` is the whole AI streaming lifecycle — staleness drops, tool
 * correlation by `callId`, empty-response detection — and it had no test at
 * all, despite the file header advertising it as unit-testable.
 *
 * These drive it directly rather than through a live stream, which is the
 * point: every branch is reachable from a plain object, so none of this needs
 * a backend, a provider, or a network.
 */

/** A minimally-populated event; `gen` defaults to whatever the session is on. */
function ev(kind: string, extra: Partial<AgentEventDto> = {}): AgentEventDto {
	return { kind, gen: chat.gen, ...extra } as AgentEventDto;
}

describe("chat.applyEvent", () => {
	beforeEach(() => {
		chat.reset();
	});

	it("appends streamed text", () => {
		chat.applyEvent(ev("text", { text: "Hel" }));
		chat.applyEvent(ev("text", { text: "lo" }));
		expect(chat.text).toBe("Hello");
	});

	/**
	 * The staleness guard. A superseded run must not paint over a newer one —
	 * without this, cancelling a slow answer and asking something else shows
	 * the old answer's tokens interleaved into the new one.
	 */
	it("drops events from a superseded run", () => {
		const stale = chat.gen - 1;
		chat.applyEvent({ kind: "text", gen: stale, text: "from the old run" } as AgentEventDto);
		expect(chat.text).toBe("");
	});

	it("keeps events from the current run", () => {
		chat.applyEvent(ev("text", { text: "current" }));
		expect(chat.text).toBe("current");
	});

	describe("tool steps", () => {
		it("records a started tool as running", () => {
			chat.applyEvent(ev("tool_started", { call_id: "c1", tool_name: "web", tool_args: "{}" }));
			expect(chat.toolSteps).toHaveLength(1);
			expect(chat.toolSteps[0]).toMatchObject({
				callId: "c1",
				name: "web",
				status: "running",
			});
		});

		/**
		 * Correlation is by `callId`, not by position — tools complete out of
		 * order, so matching on array index would attach a result to the wrong
		 * step.
		 */
		it("completes the matching step when results arrive out of order", () => {
			chat.applyEvent(ev("tool_started", { call_id: "a", tool_name: "first" }));
			chat.applyEvent(ev("tool_started", { call_id: "b", tool_name: "second" }));

			chat.applyEvent(ev("tool_completed", { call_id: "b", text: "b-output" }));

			const [a, b] = chat.toolSteps;
			expect(a).toMatchObject({ callId: "a", status: "running" });
			expect(b).toMatchObject({ callId: "b", status: "done", output: "b-output" });
		});

		it("marks a failed tool as failed", () => {
			chat.applyEvent(ev("tool_started", { call_id: "c1", tool_name: "run" }));
			chat.applyEvent(ev("tool_failed", { call_id: "c1", text: "boom" }));
			expect(chat.toolSteps[0]).toMatchObject({ status: "failed", output: "boom" });
		});

		it("ignores a completion for an unknown call id", () => {
			chat.applyEvent(ev("tool_started", { call_id: "known", tool_name: "run" }));
			chat.applyEvent(ev("tool_completed", { call_id: "nobody", text: "stray" }));
			expect(chat.toolSteps).toHaveLength(1);
			expect(chat.toolSteps[0]).toMatchObject({ callId: "known", status: "running" });
		});
	});

	describe("final", () => {
		it("stops streaming and keeps the streamed text", () => {
			chat.applyEvent(ev("text", { text: "answer" }));
			chat.applyEvent(ev("final", {}));
			expect(chat.streaming).toBe(false);
			expect(chat.text).toBe("answer");
			expect(chat.error).toBeFalsy();
		});

		/**
		 * The known small-model quirk: a `final` with nothing streamed and no
		 * tool output would otherwise render a blank card the user cannot act
		 * on.
		 */
		it("reports an empty response rather than showing a blank card", () => {
			chat.applyEvent(ev("final", {}));
			expect(chat.error).toContain("empty response");
		});

		it("does NOT report empty when a tool produced output", () => {
			chat.applyEvent(ev("tool_started", { call_id: "c1", tool_name: "run" }));
			chat.applyEvent(ev("tool_completed", { call_id: "c1", text: "result" }));
			chat.applyEvent(ev("final", {}));
			expect(chat.error).toBeFalsy();
		});

		it("carries the truncated flag", () => {
			chat.applyEvent(ev("final", { text: "cut off", truncated: true }));
			expect(chat.truncated).toBe(true);
		});
	});

	it("pauses for approval and records what is being asked", () => {
		chat.applyEvent(
			ev("awaiting_approval", {
				call_id: "c1",
				tool_name: "run",
				tool_args: "rm -rf /tmp/x",
				reason: "destructive",
			}),
		);
		expect(chat.streaming).toBe(false);
		expect(chat.approval).toMatchObject({
			callId: "c1",
			toolName: "run",
			reason: "destructive",
		});
	});

	it("accumulates token usage across events", () => {
		chat.applyEvent(ev("usage", { input_tokens: 10, output_tokens: 4 }));
		chat.applyEvent(ev("usage", { input_tokens: 5, output_tokens: 1 }));
		expect(chat.tokensIn).toBe(15);
		expect(chat.tokensOut).toBe(5);
	});

	it("surfaces stop and error as messages, not silence", () => {
		chat.applyEvent(ev("stopped", { text: "Cancelled." }));
		expect(chat.streaming).toBe(false);
		expect(chat.error).toBe("Cancelled.");

		chat.reset();
		chat.applyEvent(ev("error", {}));
		expect(chat.error).toBeTruthy();
	});

	it("ignores events it has no UI for", () => {
		chat.applyEvent(ev("turn_started", {}));
		chat.applyEvent(ev("reasoning", { text: "thinking" }));
		expect(chat.text).toBe("");
		expect(chat.error).toBeFalsy();
	});
});

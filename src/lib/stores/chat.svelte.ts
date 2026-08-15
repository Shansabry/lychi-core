/**
 * ChatSession — all AI-conversation state + the agent-event application logic.
 *
 * Extracted from +page.svelte (Phase 2 of the FE re-architecture). Owns the ~15
 * `ai*` fields, the streamed-answer transcript, tool-step lifecycle, the pending
 * destructive-tool approval, and the token/truncation bookkeeping. The launcher
 * page keeps only thin orchestration wrappers (the parts that also touch page
 * state like `lastResult` or call `runCommand`).
 *
 * CRITICAL: `gen` is a plain NON-reactive field (never `$state`). It's the async
 * staleness guard — bumped on every new run so late `lychi://agent-event`s from a
 * superseded run are dropped in `applyEvent`. Making it reactive would re-trigger
 * effects on every bump and (worse) is unnecessary churn; it's deliberately opaque
 * to reactivity. Do not add `$state` to it.
 *
 * Arrays that are replaced wholesale (`turns`, `toolSteps`) use `$state.raw` — a
 * perf win (no deep proxying) that matches the WebKit paint sensitivity.
 *
 * Svelte 5 note: methods are arrow-function fields so `this` stays bound when they
 * are passed as component callback props.
 */

import {
	type AgentEventDto,
	agentApprove,
	agentChatStart,
	cancelAiChat,
	type MessageDisplay,
} from "$lib/ipc";
import { attachments } from "./attachments.svelte";
import { ui } from "./ui.svelte";

export type ToolStep = {
	callId: string;
	name: string;
	args: string;
	status: "running" | "done" | "failed";
	output?: string;
	/** A rich result to render inline (e.g. a QR svg, a weather card). */
	artifactKind?: string;
	artifactContent?: string;
};

/**
 * A large payload folded out of a user message into a collapsed chip. Presets
 * that act on a selection/paste (`summarize`, `translate <blob>`) attach the
 * blob here instead of dumping it inline — the model still gets the full text,
 * the bubble shows `label` and expands `body` on click. Mirrors Raycast/ChatGPT.
 */
export type UserAttachment = { label: string; body: string };

/**
 * A file that was attached to a turn, kept only so the bubble can show which
 * files that turn was asked about. A display record — the content already went
 * to the model (inlined text or a vision block), so this is never re-sent.
 */
export type TurnFile = { name: string; thumbnail: string | null };

/** A completed prior turn in the conversation (shown above the streaming answer). */
export type AiTurn = {
	user: string;
	text: string;
	toolSteps: ToolStep[];
	attachment?: UserAttachment;
	files?: TurnFile[];
};

// --- System prompts (moved here with the chat logic that uses them) ---

/**
 * The system prompt for the agent — a TOOL CONTRACT, not a persona. A launcher is
 * an instrument you invoke dozens of times a day, so it defines behaviour (terse,
 * act-then-report) rather than a chatbot character.
 *
 * ALL tool knowledge (names, args, when-to-use, tool-choice conduct) lives in the
 * generated capability manifest the backend appends to this prompt, built from the
 * live registry — one source of truth, no drift.
 */
export const AGENT_SYSTEM =
	"You are the AI inside Lychi, a Linux launcher. Be terse and direct — no preamble or sign-off. Answer in minimal markdown. Act via tools when useful; otherwise answer the question directly.";

/** System prompt for AI presets (text transforms) — do the task, nothing else. */
export const PRESET_SYSTEM =
	"You are Lychi's AI command runner. Perform the task in the user's message directly and return only the result — no preamble, no explanation, no tool use. Use minimal markdown.";

class ChatSession {
	// Streamed answer + status.
	text = $state("");
	streaming = $state(false);
	error = $state<string | null>(null);

	// Transcript + live turn. Replaced wholesale → $state.raw.
	turns = $state.raw<AiTurn[]>([]);
	toolSteps = $state.raw<ToolStep[]>([]);
	lastUser = $state("");
	// A big payload folded out of the current user message into a collapsed chip.
	lastAttachment = $state<UserAttachment | null>(null);
	// Files attached to the live turn, shown as chips on the user bubble.
	lastFiles = $state.raw<TurnFile[]>([]);

	// Pending destructive-tool approval (loop suspended until decided).
	approval = $state<{ callId: string; toolName: string; args: string; reason: string } | null>(
		null,
	);

	// In-panel follow-up reply box.
	reply = $state("");

	// Token spend + truncation notice.
	truncated = $state(false);
	tokensIn = $state(0);
	tokensOut = $state(0);
	tokensCached = $state(0);

	/**
	 * The conversation was PRESERVED across a re-summon because a run was still
	 * active when the user clicked away (see `shouldResume`). Drives the "⚡
	 * Resumed your last run" banner. Transient: cleared on the next explicit
	 * action (Start fresh, a follow-up, cancel, or a fresh reset).
	 */
	resumed = $state(false);

	/** NON-reactive staleness guard (see file header). Bumped per run. */
	gen = 0;

	/**
	 * Everything needed to re-run the last turn verbatim.
	 *
	 * NOT `lastUser` — that's the DISPLAY text, which for a preset or an
	 * attachment turn deliberately differs from what the model received
	 * (instruction-only, `@`-expanded). Regenerating from the bubble would send a
	 * different prompt than the one that failed. Non-reactive: it only feeds an
	 * action, never renders.
	 */
	#lastRun: {
		system: string;
		prompt: string;
		withTools: boolean;
		images: string[];
		display?: string;
		attachment?: UserAttachment | null;
		files?: TurnFile[];
	} | null = null;

	/** Whether there's a turn to regenerate (drives the button). */
	get canRegenerate(): boolean {
		return this.#lastRun !== null;
	}

	/**
	 * Reset the fields shared by every run start (not the transcript). `display`
	 * overrides the on-screen bubble text and `attachment` folds a big payload
	 * into a collapsed chip; either way the full prompt still goes to the model
	 * via the run starter (which passes it to `agentChatStart` independently).
	 */
	#beginRun(
		user: string,
		display?: string,
		attachment?: UserAttachment | null,
		files?: TurnFile[],
	): number {
		this.lastUser = display ?? user;
		this.lastAttachment = attachment ?? null;
		this.lastFiles = files ?? [];
		this.text = "";
		this.toolSteps = [];
		this.error = null;
		this.approval = null;
		this.truncated = false;
		this.streaming = true;
		ui.showAi();
		return ++this.gen;
	}

	/**
	 * Consume the staged attachment tray for one run: expand text-route files into
	 * `@`-refs on the prompt, collect image paths for the vision argument, and
	 * keep a display record for the bubble. Clears the tray — attachments belong
	 * to the turn that sends them, not to the session.
	 */
	#takeAttachments(prompt: string): { prompt: string; images: string[]; files: TurnFile[] } {
		if (!attachments.any) return { prompt, images: [], files: [] };
		const expanded = attachments.applyToPrompt(prompt);
		const images = attachments.imagePaths();
		const files = attachments.usable.map((a) => ({ name: a.name, thumbnail: a.thumbnail }));
		attachments.clear();
		return { prompt: expanded, images, files };
	}

	#fail(gen: number, e: unknown): void {
		if (gen === this.gen) {
			this.streaming = false;
			this.error = e instanceof Error ? e.message : String(e);
		}
	}

	/** Clear the active conversation entirely (fresh start / summon). */
	reset = (): void => {
		this.gen++; // supersede any in-flight run
		this.text = "";
		this.error = null;
		this.streaming = false;
		ui.clearAi();
		this.toolSteps = [];
		this.approval = null;
		this.turns = [];
		this.lastUser = "";
		this.lastAttachment = null;
		this.lastFiles = [];
		this.reply = "";
		this.truncated = false;
		this.tokensIn = 0;
		this.tokensOut = 0;
		this.tokensCached = 0;
		this.resumed = false;
		// The tray is per-conversation: a fresh start (or a re-summon) shouldn't
		// leave files staged from a session the user has moved on from.
		attachments.clear();
		// Nothing to regenerate once the conversation is gone.
		this.#lastRun = null;
	};

	/**
	 * Start (fresh) or continue (follow-up) a tool-calling agent run. `fresh` = new
	 * conversation; else the backend appends this as a follow-up. Progress arrives
	 * via `lychi://agent-event` → `applyEvent`.
	 */
	start = async (prompt: string, fresh = true): Promise<void> => {
		const text = prompt.trim();
		if (!text) return;
		// Consume the staged attachments: doc/text files fold into the prompt as
		// `@`-refs (the backend inlines their extracted text), images travel as
		// paths the backend base64-encodes into vision blocks. Snapshot BEFORE the
		// await so a fast second attach can't retro-apply to this turn.
		const sent = this.#takeAttachments(text);
		if (fresh) {
			this.turns = [];
		} else if (this.text) {
			// Snapshot the completed answer into the transcript before the new turn.
			this.turns = [
				...this.turns,
				{
					user: this.lastUser,
					text: this.text,
					toolSteps: this.toolSteps,
					attachment: this.lastAttachment ?? undefined,
					files: this.lastFiles,
				},
			];
		}
		// The bubble shows what the user typed; the model gets the `@`-expanded
		// prompt. Same split the presets already use for a folded selection.
		const gen = this.#beginRun(sent.prompt, text, null, sent.files);
		this.#lastRun = {
			system: AGENT_SYSTEM,
			prompt: sent.prompt,
			withTools: true,
			images: sent.images,
			display: text,
			files: sent.files,
		};
		try {
			await agentChatStart(
				AGENT_SYSTEM,
				sent.prompt,
				fresh,
				/* withTools */ true,
				gen,
				sent.images,
			);
		} catch (e) {
			this.#fail(gen, e);
		}
	};

	/**
	 * Re-run the last turn with the exact prompt the model already received.
	 *
	 * `fresh: true` deliberately — a regenerate REPLACES the previous attempt
	 * rather than appending a second copy of the same question to the session.
	 * That matters most for the case this exists for: an empty or garbled
	 * response, where a follow-up would leave the dud turn in the model's context.
	 */
	regenerate = async (): Promise<void> => {
		const last = this.#lastRun;
		if (!last || this.streaming) return;
		this.turns = [];
		const gen = this.#beginRun(last.prompt, last.display, last.attachment, last.files);
		try {
			await agentChatStart(
				last.system,
				last.prompt,
				/* fresh */ true,
				last.withTools,
				gen,
				last.images,
			);
		} catch (e) {
			this.#fail(gen, e);
		}
	};

	/** Send the in-panel reply (continues the conversation). */
	sendReply = async (): Promise<void> => {
		const text = this.reply.trim();
		if (!text || this.streaming) return;
		this.reply = "";
		this.resumed = false; // a follow-up dismisses the "resumed" banner
		await this.start(text, /* fresh */ false);
	};

	/**
	 * Run a tool-FREE preset (text transform) — streams into the full surface.
	 * The model receives the full `prompt`; `display` shapes the on-screen bubble
	 * (instruction line + an optional collapsed attachment for a big selection).
	 */
	startPreset = async (
		prompt: string,
		display?: { instruction: string; attachment?: UserAttachment | null },
	): Promise<void> => {
		const text = prompt.trim();
		if (!text) return;
		this.turns = [];
		const gen = this.#beginRun(text, display?.instruction, display?.attachment);
		this.#lastRun = {
			system: PRESET_SYSTEM,
			prompt: text,
			withTools: false,
			images: [],
			display: display?.instruction,
			attachment: display?.attachment,
		};
		// Persist the fold with the message. `presetDisplay` is the ONE decider of
		// this split; recording its verdict here is what lets a recalled
		// conversation render identically without a second, drifting resolver.
		const stored: MessageDisplay | null = display?.attachment
			? {
					instruction: display.instruction,
					label: display.attachment.label,
					body: display.attachment.body,
				}
			: null;
		try {
			await agentChatStart(
				PRESET_SYSTEM,
				text,
				/* fresh */ true,
				/* withTools */ false,
				gen,
				[],
				stored,
			);
		} catch (e) {
			this.#fail(gen, e);
		}
	};

	/** Resolve a pending destructive-tool approval and resume the agent. */
	approve = async (ok: boolean): Promise<void> => {
		if (!this.approval) return;
		this.approval = null;
		this.truncated = false;
		this.streaming = true;
		const gen = this.gen; // same run continues
		try {
			await agentApprove(ok, gen);
		} catch (e) {
			this.#fail(gen, e);
		}
	};

	/** Cancel an in-flight agent run (Esc while streaming). */
	cancel = (): void => {
		this.gen++; // drop late events on the frontend
		this.streaming = false;
		this.resumed = false; // cancelling ends the run — no "resumed" cue to keep
		cancelAiChat().catch(() => {});
	};

	/**
	 * Load a recalled conversation's turns onto the surface (no new request). The
	 * caller supplies the paired turns + the last (live) turn.
	 */
	loadRecalled = (
		priorTurns: AiTurn[],
		liveUser: string,
		liveText: string,
		liveAttachment?: UserAttachment,
	): void => {
		this.turns = priorTurns;
		this.lastUser = liveUser;
		this.lastAttachment = liveAttachment ?? null;
		this.lastFiles = [];
		this.text = liveText;
		this.toolSteps = [];
		this.error = null;
		this.approval = null;
		this.streaming = false;
		ui.showAi();
		this.gen++; // fresh generation for any follow-up
	};

	/**
	 * Apply one `lychi://agent-event`. Pure w.r.t. Tauri (feed it a DTO), so the
	 * whole streaming lifecycle is unit-testable. Drops stale-generation events.
	 */
	applyEvent = (ev: AgentEventDto): void => {
		if (ev.gen !== this.gen) return; // stale run — ignore
		switch (ev.kind) {
			case "text":
				this.text += ev.text ?? "";
				break;
			case "tool_started":
				this.toolSteps = [
					...this.toolSteps,
					{
						callId: ev.call_id ?? "",
						name: ev.tool_name ?? "",
						args: ev.tool_args ?? "",
						status: "running",
					},
				];
				break;
			case "tool_output_delta": {
				// A live chunk from a still-running tool — append it to the
				// matching step's output so the chat block fills in real time.
				// The final tool_completed carries the full output and replaces
				// this (idempotent: it equals what we streamed), so a dropped or
				// out-of-order delta self-heals at completion.
				const chunk = ev.text ?? "";
				this.toolSteps = this.toolSteps.map((s) =>
					s.callId === ev.call_id ? { ...s, output: (s.output ?? "") + chunk } : s,
				);
				break;
			}
			case "tool_completed":
			case "tool_failed": {
				const status = ev.kind === "tool_failed" ? "failed" : "done";
				this.toolSteps = this.toolSteps.map((s) =>
					s.callId === ev.call_id
						? {
								...s,
								status,
								output: ev.text,
								artifactKind: ev.artifact_kind,
								artifactContent: ev.artifact_content,
							}
						: s,
				);
				break;
			}
			case "awaiting_approval":
				this.streaming = false;
				this.approval = {
					callId: ev.call_id ?? "",
					toolName: ev.tool_name ?? "",
					args: ev.tool_args ?? "",
					reason: ev.reason ?? "This action needs your approval.",
				};
				break;
			case "final":
				this.streaming = false;
				if (ev.text) this.text = ev.text;
				this.truncated = ev.truncated ?? false;
				// Empty answer with nothing streamed and no tool output — the model
				// returned no content (a known small-model/Groq quirk on some inputs).
				// Never leave a blank card: surface it so the user can retry/rephrase.
				if (!this.text.trim() && this.toolSteps.length === 0) {
					this.error = "The model returned an empty response. Try rephrasing.";
				}
				break;
			case "usage":
				this.tokensIn += ev.input_tokens ?? 0;
				this.tokensOut += ev.output_tokens ?? 0;
				this.tokensCached += ev.cached_input_tokens ?? 0;
				break;
			case "stopped":
				this.streaming = false;
				this.error = ev.text ?? "Stopped.";
				break;
			case "error":
				this.streaming = false;
				this.error = ev.text ?? "The agent failed.";
				break;
			// turn_started / reasoning: no UI change for now.
		}
	};
}

/**
 * Whether a re-summon should PRESERVE the conversation (and offer to continue)
 * rather than reset to a fresh launcher.
 *
 * True only while a run is genuinely IN PROGRESS — the agent is streaming, or it
 * is paused awaiting a destructive-tool approval. In both cases the backend task
 * is still alive (hiding never cancels it), so preserving the frontend surface
 * lets its events keep landing and the user pick up where they were. A FINISHED
 * answer or an empty launcher is not "in progress" and resets as before.
 *
 * Pure and dependency-light (reads two fields) so the summon-path decision is
 * unit-testable without a DOM.
 */
export function shouldResume(c: { streaming: boolean; approval: unknown | null }): boolean {
	return c.streaming || c.approval !== null;
}

/** The single app-wide chat session. */
export const chat = new ChatSession();

<script lang="ts">
import { Globe, Sparkles } from "lucide-svelte";
import { renderMarkdown } from "$lib/markdown";
import { sanitizeSvg } from "$lib/sanitize";

type ToolStep = {
	callId: string;
	name: string;
	args: string;
	status: "running" | "done" | "failed";
	output?: string;
	artifactKind?: string;
	artifactContent?: string;
};
type Approval = { callId: string; toolName: string; args: string; reason: string };
type Turn = { user: string; text: string; toolSteps: ToolStep[] };

let {
	/** Completed prior turns in this conversation (shown above the live answer). */
	turns = [] as Turn[],
	/** The current turn's user message — shown as a bubble above the live answer. */
	lastUser = "",
	/** The assistant text for the current turn (streams in). */
	text = "",
	/** Whether tokens are still arriving (shows a blinking cursor + Stop). */
	streaming = false,
	/** An error message, if the stream failed. */
	error = null,
	/** Tool calls the agent has run this turn. */
	toolSteps = [] as ToolStep[],
	/** A pending destructive-tool approval prompt (loop suspended). */
	approval = null as Approval | null,
	/** The follow-up reply box text (bindable). */
	reply = $bindable(""),
	/** Called with the user's approve/reject decision. */
	onapprove,
	/** Called to send the follow-up reply. */
	onreply,
	/** Called to stop an in-flight answer. */
	onstop,
	/** Quick-AI fork card: show [Search web] / [Full chat] instead of a reply box. */
	quick = false,
	/** Fork-card: bail out to a plain web search. */
	onwebsearch,
	/** Fork-card: escalate to the full tool-calling agent. */
	onfullchat,
	/** The answer was cut off at the token cap — show a truncation notice. */
	truncated = false,
	/** Accumulated token spend for the conversation. */
	tokensIn = 0,
	tokensOut = 0,
}: {
	turns?: Turn[];
	lastUser?: string;
	text?: string;
	streaming?: boolean;
	error?: string | null;
	toolSteps?: ToolStep[];
	approval?: Approval | null;
	reply?: string;
	onapprove?: (approve: boolean) => void;
	onreply?: () => void;
	onstop?: () => void;
	quick?: boolean;
	onwebsearch?: () => void;
	onfullchat?: () => void;
	truncated?: boolean;
	tokensIn?: number;
	tokensOut?: number;
} = $props();

const md = renderMarkdown;
let html = $derived(md(text));

// Auto-scroll: keep the transcript pinned to the bottom as the answer streams
// and as follow-up turns are added — but only when the user is already near the
// bottom. If they've scrolled up to re-read, don't yank them back down.
let transcriptEl: HTMLDivElement | undefined = $state();
let stick = $state(true);
function onTranscriptScroll() {
	if (!transcriptEl) return;
	const { scrollTop, scrollHeight, clientHeight } = transcriptEl;
	// Within ~40px of the bottom counts as "following along".
	stick = scrollHeight - (scrollTop + clientHeight) < 40;
}
// A fresh generation re-pins to the bottom even if the user had scrolled up in
// a previous answer. `streaming` flipping true (with no text yet) marks the
// start of a new answer.
let wasStreaming = $state(false);
$effect(() => {
	if (streaming && !wasStreaming) stick = true;
	wasStreaming = streaming;
});
// Re-runs whenever the streamed text / turns / tool steps change.
$effect(() => {
	// Touch the reactive inputs so the effect tracks them.
	void text;
	void turns.length;
	void toolSteps.length;
	if (!transcriptEl || !stick) return;
	// After the DOM updates, jump to the bottom.
	requestAnimationFrame(() => {
		if (transcriptEl) transcriptEl.scrollTop = transcriptEl.scrollHeight;
	});
});

// The reply box only shows when idle (not streaming, no pending approval) and
// there's an answer to follow up on. Enter sends; the input keeps focus.
// Focus does NOT auto-shift here — the launcher input stays primary. It's
// shifted explicitly (via `focusReply`) only when the user acts: clicks
// "Full chat" or opens a recalled conversation.
let replyEl: HTMLInputElement | undefined = $state();

/** Move focus to the reply box. Called from the parent on explicit user intent. */
export function focusReply() {
	requestAnimationFrame(() => replyEl?.focus());
}
function onReplyKeydown(e: KeyboardEvent) {
	if (e.key === "Enter" && !e.shiftKey) {
		e.preventDefault();
		e.stopPropagation();
		onreply?.();
	}
}

// Keyboard shortcuts for the AI answer's action buttons, so they're not
// mouse-only:
//   - Approval prompt (Approve/Reject): ⌘/Ctrl+Enter or Y = approve,
//     Escape or N = reject. This is a decision point, so it takes priority.
//   - Fork card (Search web / Full chat): ⌘/Ctrl+Enter = search web,
//     Enter = full chat (mirrors the button kbd hints).
function onWindowKeydown(e: KeyboardEvent) {
	if (approval) {
		const key = e.key.toLowerCase();
		if ((e.metaKey || e.ctrlKey) && e.key === "Enter") {
			e.preventDefault();
			e.stopPropagation();
			onapprove?.(true);
		} else if (key === "y") {
			e.preventDefault();
			e.stopPropagation();
			onapprove?.(true);
		} else if (key === "n" || e.key === "Escape") {
			e.preventDefault();
			e.stopPropagation();
			onapprove?.(false);
		}
		return;
	}
	// Fork card is showing (idle quick answer with buttons).
	if (quick && !streaming) {
		if ((e.metaKey || e.ctrlKey) && e.key === "Enter") {
			e.preventDefault();
			e.stopPropagation();
			onwebsearch?.();
		} else if (e.key === "Enter") {
			e.preventDefault();
			e.stopPropagation();
			onfullchat?.();
		}
	}
}
</script>

<svelte:window onkeydown={onWindowKeydown} />

{#snippet toolStepsView(steps: ToolStep[])}
	{#if steps.length > 0}
		<div class="tool-steps">
			{#each steps as step (step.callId)}
				<div class="tool-step" class:failed={step.status === "failed"}>
					<span class="tool-icon">
						{#if step.status === "running"}⟳{:else if step.status === "failed"}✗{:else}✓{/if}
					</span>
					<span class="tool-name">{step.name}</span>
					<span class="tool-args">{step.args}</span>
				</div>
				{#if step.artifactKind === "svg" && step.artifactContent}
					<!-- Inline rich tool output (e.g. a QR code). Sanitized SVG. -->
					<!-- eslint-disable-next-line svelte/no-at-html-tags — sanitized -->
					<div class="tool-artifact svg">{@html sanitizeSvg(step.artifactContent)}</div>
				{/if}
			{/each}
		</div>
	{/if}
{/snippet}

<div class="ai-chat">
	<div class="ai-transcript" bind:this={transcriptEl} onscroll={onTranscriptScroll}>
		<!-- Prior turns in this conversation. -->
		{#each turns as turn, i (i)}
			<div class="user-turn">{turn.user}</div>
			{@render toolStepsView(turn.toolSteps)}
			<!-- eslint-disable-next-line svelte/no-at-html-tags — sanitized -->
			<div class="ai-md turn">{@html md(turn.text)}</div>
		{/each}

		<!-- The current turn: the user's question, then tool steps + the answer. -->
		{#if lastUser}
			<div class="user-turn">{lastUser}</div>
		{/if}
		{@render toolStepsView(toolSteps)}

		{#if error}
			<div class="ai-error">{error}</div>
		{:else if text}
			<!-- eslint-disable-next-line svelte/no-at-html-tags — sanitized above -->
			<div class="ai-md">{@html html}</div>{#if streaming && !approval}<span class="cursor" aria-hidden="true"></span>{/if}
		{:else if streaming && !approval}
			<div class="thinking"><span class="cursor" aria-hidden="true"></span></div>
		{/if}

		{#if truncated && !streaming}
			<div class="truncated-note">
				⚠ Response hit the token limit and was cut off. Raise “Max Tokens” in Settings → AI for longer answers.
			</div>
		{/if}

		{#if approval}
			<div class="approval">
				<div class="approval-reason">⚠ {approval.reason}</div>
				<div class="approval-cmd"><code>{approval.toolName} {approval.args}</code></div>
				<div class="approval-actions">
					<button class="approve" onclick={() => onapprove?.(true)} title="Approve (⌘↵ or Y)">
						Approve <kbd>⌘↵</kbd>
					</button>
					<button class="reject" onclick={() => onapprove?.(false)} title="Reject (Esc or N)">
						Reject <kbd>Esc</kbd>
					</button>
				</div>
			</div>
		{/if}
	</div>

	<!-- Footer. Hidden during an approval (the user is deciding, not chatting).
	     - streaming        → Stop button
	     - quick fork card  → [Search web] / [Full chat] buttons
	     - full agent chat  → the follow-up reply box -->
	{#if !approval}
		<div class="ai-footer">
			{#if streaming}
				<button class="stop-btn" onclick={() => onstop?.()}>■ Stop</button>
			{:else if quick}
				<button class="fork-btn" onclick={() => onwebsearch?.()} title="Search the web (⌘↵)">
					<Globe size={14} strokeWidth={2} />
					<span>Search web</span>
					<kbd>⌘↵</kbd>
				</button>
				<button
					class="fork-btn primary"
					onclick={() => onfullchat?.()}
					title="Continue in full chat (↵)"
				>
					<Sparkles size={14} strokeWidth={2} />
					<span>Full chat</span>
					<kbd>↵</kbd>
				</button>
			{:else}
				<input
					class="reply-input"
					type="text"
					placeholder="Ask a follow-up…"
					bind:value={reply}
					bind:this={replyEl}
					onkeydown={onReplyKeydown}
				/>
				<button class="send-btn" onclick={() => onreply?.()} disabled={!reply.trim()}>↵</button>
			{/if}
		</div>
	{/if}

	{#if tokensOut > 0 && !quick}
		<div class="token-spend" title="Token spend this conversation">
			{tokensIn.toLocaleString()} in · {tokensOut.toLocaleString()} out · {(tokensIn + tokensOut).toLocaleString()} tokens
		</div>
	{/if}
</div>

<style>
	.ai-chat {
		display: flex;
		flex-direction: column;
	}
	.ai-transcript {
		/* Fixed height with internal scroll — the window stays a predictable size;
		   the answer scrolls rather than growing the overlay (avoids Wayland
		   resize jank). */
		max-height: 320px;
		overflow-y: auto;
		padding: 14px 20px;
		font-family: var(--font-sans, system-ui);
		font-size: 14px;
		line-height: 1.55;
		color: var(--fg);
	}

	.ai-error {
		color: var(--error);
		font-size: 13px;
		font-family: var(--font-mono);
	}

	.truncated-note {
		margin-top: 10px;
		padding: 7px 10px;
		border-radius: 6px;
		border: 1px solid color-mix(in srgb, #d0b060 40%, var(--border));
		background: color-mix(in srgb, #d0b060 10%, transparent);
		color: #d0b060;
		font-size: 12px;
		line-height: 1.45;
	}

	.token-spend {
		padding: 4px 14px 8px;
		font-family: var(--font-mono);
		font-size: 10.5px;
		color: var(--fg-muted);
		text-align: right;
	}

	/* A prior user turn — a right-aligned pill above its answer. */
	.user-turn {
		font-size: 13px;
		color: var(--fg);
		background: var(--bg-secondary);
		border-radius: 8px;
		padding: 5px 10px;
		margin: 10px 0 8px auto;
		max-width: 85%;
		width: fit-content;
	}
	.ai-md.turn {
		display: block;
		margin-bottom: 6px;
	}

	/* Footer: reply input / stop button, pinned below the scroll area. */
	.ai-footer {
		display: flex;
		align-items: center;
		gap: 6px;
		padding: 8px 14px;
		border-top: 1px solid var(--border);
	}
	.reply-input {
		flex: 1;
		background: var(--bg-secondary);
		border: 1px solid var(--border);
		border-radius: 6px;
		padding: 6px 10px;
		font-family: var(--font-mono);
		font-size: 12.5px;
		color: var(--fg);
		outline: none;
	}
	.reply-input:focus {
		border-color: var(--accent);
	}
	.send-btn,
	.stop-btn {
		font-family: var(--font-mono);
		font-size: 12px;
		padding: 6px 12px;
		border-radius: 6px;
		border: 1px solid var(--border);
		background: var(--bg-secondary);
		color: var(--fg);
		cursor: pointer;
	}
	.send-btn:disabled {
		opacity: 0.4;
		cursor: default;
	}
	.stop-btn {
		flex: 1;
		color: var(--error);
		border-color: color-mix(in srgb, var(--error) 40%, var(--border));
	}
	.stop-btn:hover {
		background: color-mix(in srgb, var(--error) 12%, var(--bg-secondary));
	}

	/* Fork-card buttons: [Search web] / [Full chat]. Icon + label centered,
	   with a right-aligned keyboard hint. */
	.fork-btn {
		flex: 1;
		display: inline-flex;
		align-items: center;
		justify-content: center;
		gap: 7px;
		font-family: var(--font-sans, system-ui);
		font-size: 12.5px;
		font-weight: 500;
		line-height: 1;
		padding: 8px 12px;
		border-radius: 6px;
		border: 1px solid var(--border);
		background: var(--bg-secondary);
		color: var(--fg);
		cursor: pointer;
	}
	.fork-btn :global(svg) {
		flex-shrink: 0;
	}
	.fork-btn kbd {
		font-family: var(--font-mono);
		font-size: 10px;
		line-height: 1;
		padding: 2px 4px;
		border-radius: 3px;
		background: color-mix(in srgb, var(--fg) 10%, transparent);
		color: var(--fg-muted);
	}
	.fork-btn:hover {
		background: color-mix(in srgb, var(--fg) 8%, var(--bg-secondary));
	}
	.fork-btn.primary {
		border-color: var(--accent);
		color: var(--accent);
	}
	.fork-btn.primary kbd {
		background: color-mix(in srgb, var(--accent) 18%, transparent);
		color: var(--accent);
	}
	.fork-btn.primary:hover {
		background: var(--accent);
		color: var(--bg);
	}
	.fork-btn.primary:hover kbd {
		background: color-mix(in srgb, var(--bg) 25%, transparent);
		color: var(--bg);
	}

	/* The rendered markdown. `:global` because the HTML is injected, so scoped
	   selectors wouldn't reach it. Scoped under .ai-md to avoid leaking. */
	.ai-md {
		display: inline;
	}
	.ai-md :global(p) {
		margin: 0 0 0.7em;
	}
	.ai-md :global(p:last-child) {
		margin-bottom: 0;
		display: inline;
	}
	.ai-md :global(pre) {
		position: relative;
		background: var(--bg-secondary);
		border: 1px solid var(--border);
		border-radius: 8px;
		padding: 11px 13px;
		overflow-x: auto;
		font-family: var(--font-mono);
		font-size: 12.5px;
		line-height: 1.5;
		margin: 0.7em 0;
	}
	/* Language chip in the corner of a code block (from `data-lang`). */
	.ai-md :global(pre[data-lang])::before {
		content: attr(data-lang);
		position: absolute;
		top: 0;
		right: 0;
		padding: 2px 7px;
		font-size: 9.5px;
		text-transform: uppercase;
		letter-spacing: 0.4px;
		color: var(--fg-muted);
		background: color-mix(in srgb, var(--fg) 6%, transparent);
		border-bottom-left-radius: 6px;
	}
	.ai-md :global(pre code) {
		display: block;
		padding-top: 2px;
	}
	.ai-md :global(code) {
		font-family: var(--font-mono);
		font-size: 0.9em;
		background: var(--bg-secondary);
		padding: 0.1em 0.35em;
		border-radius: 4px;
	}
	.ai-md :global(pre code) {
		background: none;
		padding: 0;
	}
	.ai-md :global(ul),
	.ai-md :global(ol) {
		margin: 0.4em 0;
		padding-left: 1.4em;
	}
	.ai-md :global(li) {
		margin: 0.2em 0;
	}
	.ai-md :global(a) {
		color: var(--accent);
		text-decoration: none;
	}
	.ai-md :global(a:hover) {
		text-decoration: underline;
	}
	.ai-md :global(h1),
	.ai-md :global(h2),
	.ai-md :global(h3) {
		font-size: 1.05em;
		font-weight: 600;
		margin: 0.6em 0 0.3em;
	}
	.ai-md :global(blockquote) {
		border-left: 3px solid var(--border);
		margin: 0.5em 0;
		padding-left: 0.8em;
		color: var(--fg-muted);
	}

	/* Tool-call steps — a compact row per tool the agent ran. */
	.tool-steps {
		display: flex;
		flex-direction: column;
		gap: 4px;
		margin-bottom: 10px;
	}
	/* Inline rich tool output — e.g. a QR code. White plate so a dark-theme
	   QR stays scannable; capped size, centered. */
	.tool-artifact.svg {
		align-self: flex-start;
		margin: 6px 0 4px;
		padding: 10px;
		background: #fff;
		border-radius: 8px;
		border: 1px solid var(--border);
	}
	.tool-artifact.svg :global(svg) {
		display: block;
		width: 160px;
		height: 160px;
	}
	.tool-step {
		display: flex;
		align-items: center;
		gap: 7px;
		font-family: var(--font-mono);
		font-size: 12px;
		color: var(--fg-muted);
	}
	.tool-step.failed {
		color: var(--error);
	}
	.tool-icon {
		width: 14px;
		text-align: center;
		flex-shrink: 0;
	}
	.tool-name {
		color: var(--accent);
		flex-shrink: 0;
	}
	.tool-args {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.thinking {
		min-height: 1.2em;
	}

	/* Destructive-tool approval prompt. */
	.approval {
		margin-top: 12px;
		padding: 10px 12px;
		border: 1px solid rgba(208, 176, 96, 0.3);
		background: rgba(208, 176, 96, 0.08);
		border-radius: 6px;
	}
	.approval-reason {
		color: #d0b060;
		font-size: 12.5px;
		margin-bottom: 6px;
	}
	.approval-cmd {
		font-family: var(--font-mono);
		font-size: 12px;
		margin-bottom: 8px;
		overflow-x: auto;
	}
	.approval-cmd code {
		background: var(--bg-secondary);
		padding: 3px 6px;
		border-radius: 4px;
	}
	.approval-actions {
		display: flex;
		gap: 8px;
	}
	.approval-actions button {
		display: inline-flex;
		align-items: center;
		gap: 6px;
		font-family: var(--font-mono);
		font-size: 12px;
		padding: 4px 12px;
		border-radius: 4px;
		border: 1px solid var(--border);
		background: var(--bg-secondary);
		color: var(--fg);
		cursor: pointer;
	}
	.approval-actions kbd {
		font-family: var(--font-mono);
		font-size: 9.5px;
		padding: 1px 4px;
		border-radius: 3px;
		background: color-mix(in srgb, var(--fg) 12%, transparent);
		color: var(--fg-muted);
	}
	.approval-actions .approve {
		border-color: var(--accent);
		color: var(--accent);
	}
	.approval-actions .approve:hover {
		background: var(--accent);
		color: var(--bg);
	}
	.approval-actions .reject:hover {
		border-color: var(--error);
		color: var(--error);
	}

	/* Blinking cursor while streaming — inline, right after the text. */
	.cursor {
		display: inline-block;
		width: 7px;
		height: 1em;
		margin-left: 1px;
		vertical-align: text-bottom;
		background: var(--accent);
		animation: blink 1s steps(2, start) infinite;
	}
	@keyframes blink {
		0%,
		50% {
			opacity: 1;
		}
		50.01%,
		100% {
			opacity: 0;
		}
	}
</style>

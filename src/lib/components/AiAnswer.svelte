<script lang="ts">
import { Check, Copy, FolderOpen, RefreshCw, TriangleAlert } from "lucide-svelte";
import { answerRevealPath } from "$lib/answerActions";
import { getComboString, matchesAction } from "$lib/keybindings";
import { renderMarkdown, renderStreamingMarkdown } from "$lib/markdown";
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
type Attachment = { label: string; body: string };
/** A file attached to a turn — display only; its content already went to the model. */
type TurnFile = { name: string; thumbnail: string | null };
type Turn = {
	user: string;
	text: string;
	toolSteps: ToolStep[];
	attachment?: Attachment;
	files?: TurnFile[];
};

let {
	/** Completed prior turns in this conversation (shown above the live answer). */
	turns = [] as Turn[],
	/** The current turn's user message — shown as a bubble above the live answer. */
	lastUser = "",
	/** A big payload folded out of the current user message into a collapsed chip. */
	lastAttachment = null as Attachment | null,
	/** Files attached to the current turn — shown as chips under the user bubble. */
	lastFiles = [] as TurnFile[],
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
	/** Re-run the last question unchanged (empty/garbled answer, or just retry). */
	onregenerate,
	/** Reveal a path (the "Open folder" action) the answer produced, in the file
	 * manager. Given the path string detected in the answer text. */
	onreveal,
	/** This conversation was preserved across a re-summon (a run was still active).
	 * Shows the "Resumed your last run" banner with a Start-fresh escape. */
	resumed = false,
	/** Discard the resumed conversation and go back to a fresh launcher. */
	onstartfresh,
	/** The answer was cut off at the token cap — show a truncation notice. */
	truncated = false,
	/** Accumulated token spend for the conversation. */
	tokensIn = 0,
	tokensOut = 0,
}: {
	turns?: Turn[];
	lastUser?: string;
	lastAttachment?: Attachment | null;
	lastFiles?: TurnFile[];
	text?: string;
	streaming?: boolean;
	error?: string | null;
	toolSteps?: ToolStep[];
	approval?: Approval | null;
	reply?: string;
	onapprove?: (approve: boolean) => void;
	onreply?: () => void;
	onstop?: () => void;
	onregenerate?: () => void;
	onreveal?: (path: string) => void;
	resumed?: boolean;
	onstartfresh?: () => void;
	truncated?: boolean;
	tokensIn?: number;
	tokensOut?: number;
} = $props();

const md = renderMarkdown;

// Rendering the streamed answer is the WebKitGTK hot path. `md()` runs
// marked + highlight.js + DOMPurify over the WHOLE accumulated text, and the
// naive `$derived(md(text))` re-ran it on EVERY token — O(n²) across a long
// answer, and each pass re-highlights every code block already on screen.
//
// So THROTTLE while streaming: coalesce a burst of tokens into ONE render per
// animation frame. `text` still updates every token (the state is live), but we
// only re-`md()` once per frame — the eye can't see faster than that anyway. The
// moment streaming ends we render the final text SYNCHRONOUSLY, so the settled
// answer is always complete and correct (never a frame behind).
let html = $state("");
let renderScheduled = false;
$effect(() => {
	// Track the inputs so the effect re-runs when either changes.
	const currentText = text;
	const isStreaming = streaming;

	if (!isStreaming) {
		// Settled (or not streaming at all): render the FINAL text with the plain
		// renderer (no streaming repair — the answer is complete), immediately,
		// and drop any pending frame so a stale one can't overwrite it.
		renderScheduled = false;
		html = md(currentText);
		return;
	}
	// Streaming: at most one render per frame, and repair dangling markdown first
	// so a mid-token cut (unclosed code fence, half-link) doesn't flash a broken
	// layout.
	if (renderScheduled) return;
	renderScheduled = true;
	requestAnimationFrame(() => {
		renderScheduled = false;
		// Re-read the LATEST text at paint time, not the value captured when the
		// frame was scheduled — several tokens may have arrived since.
		html = renderStreamingMarkdown(text);
	});
});

// The filesystem path the answer produced (if any), so we can offer an "Open
// folder" action instead of the model's "you can open it as needed" prose. The
// detector lives in `answerActions` so this chip and the launcher's Enter
// handler agree on the actionable path — one decider, two consumers.
let revealPath = $derived(answerRevealPath(text));

// Copy-answer feedback. Holds the key of the row that just flashed "Copied",
// cleared on a timer — cheaper than a per-row boolean and self-resetting.
let copiedKey = $state<string | null>(null);
let copyTimer: ReturnType<typeof setTimeout> | undefined;
async function copyAnswer(body: string, key: string) {
	try {
		await navigator.clipboard.writeText(body);
		copiedKey = key;
		clearTimeout(copyTimer);
		copyTimer = setTimeout(() => {
			copiedKey = null;
		}, 1400);
	} catch {
		// Clipboard denied — silently no-op rather than replacing the answer
		// with an error the user can do nothing about.
	}
}

/**
 * Copy a code block when its button is clicked. The rendered markdown is
 * `{@html}`, so per-block buttons can't be Svelte components — instead one
 * delegated listener reads the `<pre>` the click landed in. Keeps the
 * sanitized-HTML boundary intact (no injected interactive markup).
 */
function onAnswerClick(e: MouseEvent) {
	const target = e.target as HTMLElement;
	const pre = target.closest("pre");
	if (!pre) return;
	// Only the top-right corner acts as the copy affordance, so selecting code
	// normally still works.
	const box = pre.getBoundingClientRect();
	if (e.clientX < box.right - 32 || e.clientY > box.top + 28) return;
	const code = pre.querySelector("code")?.textContent ?? pre.textContent ?? "";
	if (code.trim()) copyAnswer(code, `pre-${box.top}`);
}

// Which attachment chips are expanded. Keyed by a stable id per bubble
// (`turn-<i>` for prior turns, `live` for the current one).
let expanded = $state(new Set<string>());
function toggleChip(key: string) {
	// Reassign so Svelte sees the mutation.
	const next = new Set(expanded);
	if (next.has(key)) next.delete(key);
	else next.add(key);
	expanded = next;
}

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
	// The answer just SETTLED (streaming → done) and the follow-up box is now the
	// footer (not an approval prompt). Autofocus it so the
	// natural next action — asking a follow-up — needs no click. Enter still sends;
	// the empty-box + path case (Open folder) is handled in onReplyKeydown.
	if (wasStreaming && !streaming && !approval) {
		focusReply();
	}
	wasStreaming = streaming;
});

// A rotating status quip + a matching kaomoji face for the dead-air between tool
// calls / before the first token — so the wait has personality instead of a
// frozen blinking cursor. The face is paired to the joke (the shrug shrugs, the
// table-flip flips). Shuffled once per gap so it doesn't feel scripted; cycled
// every ~2.5s. Purely cosmetic; the real signal is still the Stop button.
// Faces are chosen from the battle-tested set that renders on every OS/font
// (Basic-Latin + a few ubiquitous marks): the classic shrug, Lenny, disapproval
// ಠ_ಠ, table-flip, and the (x_x) family. No exotic glyphs that box out. Each is
// paired to its joke — the shrug shrugs, the flip flips, the sleepy one sleeps.
const THINKING_VERBS: { face: string; text: string }[] = [
	{ face: "(•_•)", text: "Thinking real hard" },
	{ face: "(o_O)", text: "Consulting the rubber duck" },
	{ face: "( ͡° ͜ʖ ͡°)", text: "Bribing the compiler" },
	{ face: "ʕ•ᴥ•ʔ", text: "Summoning daemons" },
	{ face: "(⊙_⊙)", text: "Grepping the universe" },
	{ face: "(╯°□°)╯", text: "Untangling spaghetti" },
	{ face: "(ಠ_ಠ)", text: "Blaming DNS" },
	{ face: "(x_x)", text: "Sacrificing a semicolon" },
	{ face: "(⌐■_■)", text: "Reticulating splines" },
	{ face: "(^_^)", text: "Petting the penguin" },
	{ face: "¯\\_(ツ)_/¯", text: "Doing Linux things" },
	{ face: "(>_<)", text: "Overthinking it" },
	{ face: "\\(^o^)/", text: "Warming up the hamsters" },
	{ face: "(*^_^*)", text: "Aligning the stars" },
	{ face: "(¬_¬)", text: "Pretending to be busy" },
	{ face: "(・_・)", text: "Reading the manual (for once)" },
	{ face: "(￣_￣)", text: "Negotiating with sudo" },
	{ face: "(•̀_•́)", text: "Herding processes" },
	{ face: "(-_-)", text: "Waiting on the mutex" },
	{ face: "(¬‿¬)", text: "Almost definitely working" },
];
// Start on a random quip and rotate through a shuffled order, so two runs in a
// row don't open with the same word.
let verbOrder = $state(shuffled(THINKING_VERBS.length));
let verbIndex = $state(0);

// A Fisher-Yates shuffle of [0..n) — Math.random is fine here (cosmetic only).
function shuffled(n: number): number[] {
	const a = Array.from({ length: n }, (_, i) => i);
	for (let i = n - 1; i > 0; i--) {
		const j = Math.floor(Math.random() * (i + 1));
		[a[i], a[j]] = [a[j], a[i]];
	}
	return a;
}
// Show the rotating verb only in the genuine gap: streaming, no answer text yet,
// no approval prompt up. Once prose starts arriving the cursor takes over.
let showThinkingVerb = $derived(streaming && !text && !approval);
$effect(() => {
	if (!showThinkingVerb) return;
	// Fresh shuffle + start each time the gap opens, so consecutive runs don't
	// open with the same quip, then rotate through the shuffled order.
	verbOrder = shuffled(THINKING_VERBS.length);
	verbIndex = 0;
	const id = setInterval(() => {
		verbIndex = (verbIndex + 1) % verbOrder.length;
	}, 2500);
	return () => clearInterval(id);
});
// The quip (face + text) to show right now, via the shuffled order.
let thinkingVerb = $derived(THINKING_VERBS[verbOrder[verbIndex]] ?? THINKING_VERBS[0]);
// A colour for the current quip, drawn from the app's own accent set (the Guide
// category hues, which already adapt to light/dark). Cycling by verb index gives
// a fresh colour on every swap without any random source.
const THINKING_HUES = [
	"var(--cat-files)",
	"var(--cat-ai)",
	"var(--cat-web)",
	"var(--cat-system)",
	"var(--cat-developer)",
	"var(--cat-media)",
];
let thinkingHue = $derived(THINKING_HUES[verbOrder[verbIndex] % THINKING_HUES.length]);
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

// The reply box shows when idle (not streaming, no pending approval) with an
// answer to follow up on. Enter sends. It is autofocused when the answer settles
// (see the streaming-transition effect above) so a follow-up needs no click, and
// also focused on explicit intent (Full chat / opening a recalled conversation).
let replyEl: HTMLInputElement | undefined = $state();

/** Move focus to the reply box. */
export function focusReply() {
	requestAnimationFrame(() => replyEl?.focus());
}
function onReplyKeydown(e: KeyboardEvent) {
	if (matchesAction(e, "submit") && !e.shiftKey) {
		e.preventDefault();
		e.stopPropagation();
		// Empty box + the answer produced a path → Enter opens the folder (the
		// primary action) rather than sending an empty follow-up. Any typed text
		// means the user is asking a follow-up, which wins.
		if (!reply.trim() && revealPath) {
			onreveal?.(revealPath);
			return;
		}
		onreply?.();
	}
}

// Keyboard shortcuts for the AI answer's action buttons, so they're not
// mouse-only. Every binding comes from the SAME configurable table the rest of
// the app uses (`matchesAction`), and every on-screen hint is rendered from
// that table via `getComboString` — so rebinding a key in Settings moves the
// handler and the label together. Previously these were raw `e.ctrlKey` checks
// with hardcoded "⌘↵" labels, which (a) showed macOS notation in a Linux-only
// app and (b) silently ignored a rebind.
function onWindowKeydown(e: KeyboardEvent) {
	if (approval) {
		// `y`/`n` stay as raw letters: they're mnemonic accelerators for a
		// yes/no prompt, not app-wide shortcuts, and binding them would put two
		// unrebindable letters in the Settings list for no gain.
		const letter = e.key.toLowerCase();
		if (matchesAction(e, "approve_action") || letter === "y") {
			e.preventDefault();
			e.stopPropagation();
			onapprove?.(true);
		} else if (matchesAction(e, "reject_action") || letter === "n") {
			e.preventDefault();
			e.stopPropagation();
			onapprove?.(false);
		}
		return;
	}
	// NOTE: Enter-opens-folder for the main-input focus case is handled in the
	// launcher's own submit path (`handleSubmit` in +page), which is the single
	// authoritative Enter handler — doing it here too would race the input's own
	// keydown. Here we only cover the reply box (`onReplyKeydown`).
}
</script>

<svelte:window onkeydown={onWindowKeydown} />

{#snippet toolStepsView(steps: ToolStep[])}
	{#if steps.length > 0}
		<div class="tool-steps">
			{#each steps as step (step.callId)}
				{@const hasOutput = !!step.output && step.output.length > 0}
				{@const streaming = step.status === "running" && hasOutput}
				<!-- Auto-open while output is streaming in, so you watch the command
				     work live; once it's done it collapses back to a clickable row
				     unless you explicitly expanded it. A manual toggle always wins. -->
				{@const open = expanded.has(step.callId) || streaming}
				<div class="tool-step" class:failed={step.status === "failed"}>
					<!-- The row is a button when there's captured output to reveal, so
					     you can click a tool call to see exactly what the AI ran and
					     what came back. Non-interactive (a plain row) while it's still
					     running or produced nothing to show. -->
					<button
						type="button"
						class="tool-step-head"
						class:expandable={hasOutput}
						disabled={!hasOutput}
						aria-expanded={hasOutput ? open : undefined}
						onclick={() => hasOutput && toggleChip(step.callId)}
					>
						<span class="tool-icon">
							{#if step.status === "running"}⟳{:else if step.status === "failed"}✗{:else}✓{/if}
						</span>
						<span class="tool-name">{step.name}</span>
						<span class="tool-args">{step.args}</span>
						{#if hasOutput}
							<span class="tool-caret" aria-hidden="true">{open ? "▾" : "▸"}</span>
						{/if}
					</button>
					{#if hasOutput && open}
						<pre class="tool-output" class:live={streaming}>{step.output}</pre>
					{/if}
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

<!-- A large payload folded out of a user message: a collapsed chip that expands. -->
{#snippet attachmentChip(att: Attachment, key: string)}
	{@const open = expanded.has(key)}
	<div class="attachment" class:open>
		<button type="button" class="attachment-head" onclick={() => toggleChip(key)}>
			<span class="attachment-icon" aria-hidden="true">📄</span>
			<span class="attachment-label">{att.label}</span>
			<span class="attachment-caret" aria-hidden="true">{open ? "▾" : "▸"}</span>
		</button>
		{#if open}
			<div class="attachment-body">{att.body}</div>
		{/if}
	</div>
{/snippet}

<!-- Files the turn was asked about. Display only — the content already reached
     the model as a vision block or inlined text, so these are never re-sent. -->
{#snippet fileChips(files: TurnFile[])}
	<div class="turn-files">
		{#each files as f (f.name)}
			<span class="turn-file" title={f.name}>
				{#if f.thumbnail}
					<img class="turn-file-thumb" src={f.thumbnail} alt="" />
				{:else}
					<span class="turn-file-icon" aria-hidden="true">📄</span>
				{/if}
				<span class="turn-file-name">{f.name}</span>
			</span>
		{/each}
	</div>
{/snippet}

<div class="ai-chat">
	{#if resumed}
		<!-- The run was still active when the user clicked away; we kept it rather
		     than resetting. Continue is implicit (it's already restored / still
		     streaming); this is the one-click way back to a fresh launcher. -->
		<div class="resumed-banner">
			<span class="resumed-label">⚡ Resumed your last run</span>
			<button class="resumed-fresh" onclick={() => onstartfresh?.()}>Start fresh</button>
		</div>
	{/if}
	<div class="ai-transcript" bind:this={transcriptEl} onscroll={onTranscriptScroll}>
		<!-- Prior turns in this conversation. -->
		{#each turns as turn, i (i)}
			<div class="user-turn">{turn.user}</div>
			{#if turn.files && turn.files.length > 0}
				{@render fileChips(turn.files)}
			{/if}
			{#if turn.attachment}
				{@render attachmentChip(turn.attachment, `turn-${i}`)}
			{/if}
			{@render toolStepsView(turn.toolSteps)}
			<!-- eslint-disable-next-line svelte/no-at-html-tags — sanitized -->
			<div class="ai-md turn">{@html md(turn.text)}</div>
		{/each}

		<!-- The current turn: the user's question, then tool steps + the answer. -->
		{#if lastUser}
			<div class="user-turn">{lastUser}</div>
		{/if}
		{#if lastFiles.length > 0}
			{@render fileChips(lastFiles)}
		{/if}
		{#if lastAttachment}
			{@render attachmentChip(lastAttachment, "live")}
		{/if}
		{@render toolStepsView(toolSteps)}

		{#if error}
			<div class="ai-error">
				<TriangleAlert class="ai-error-icon" size={14} strokeWidth={1.75} />
				<span>{error}</span>
			</div>
		{:else if text}
			<!-- eslint-disable-next-line svelte/no-at-html-tags — sanitized above -->
			<div class="ai-md md-body" role="presentation" onclick={onAnswerClick}>{@html html}</div>{#if streaming && !approval}<span class="cursor" aria-hidden="true"></span>{/if}
		{:else if streaming && !approval}
			<div class="thinking" aria-live="polite" style:color={thinkingHue}>
				{#key verbIndex}
					<span class="thinking-face" aria-hidden="true">{thinkingVerb.face}</span>
					<span class="thinking-verb">{thinkingVerb.text}<span class="thinking-ellipsis">…</span></span>
				{/key}
			</div>
		{/if}

		<!-- Answer actions. Only once the turn is settled: mid-stream the text is
		     still changing, so copying it would capture a partial answer. -->
		{#if !streaming && !approval && (text || error)}
			<div class="answer-actions">
				{#if revealPath}
					<!-- The answer produced a path — offer to open it directly instead
					     of the model's "you can open it as needed". Primary action, so
					     Enter (with an empty follow-up box) triggers it (see +page). -->
					<button
						class="answer-action answer-action-primary"
						onclick={() => revealPath && onreveal?.(revealPath)}
						title={revealPath}
					>
						<FolderOpen size={12} strokeWidth={2} /> Open folder
						<kbd>↵</kbd>
					</button>
				{/if}
				{#if text}
					<button class="answer-action" onclick={() => copyAnswer(text, "answer")}>
						{#if copiedKey === "answer"}
							<Check size={12} strokeWidth={2} /> Copied
						{:else}
							<Copy size={12} strokeWidth={2} /> Copy
						{/if}
					</button>
				{/if}
				<!-- Regenerate re-sends the SAME question, which is the fix for an
				     empty/garbled response — the case where retyping is pure friction. -->
				<button class="answer-action" onclick={() => onregenerate?.()}>
					<RefreshCw size={12} strokeWidth={2} /> Regenerate
				</button>
			</div>
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
					<button
						class="approve"
						onclick={() => onapprove?.(true)}
						title={`Approve (${getComboString("approve_action")} or Y)`}
					>
						Approve <kbd>{getComboString("approve_action")}</kbd>
					</button>
					<button
						class="reject"
						onclick={() => onapprove?.(false)}
						title={`Reject (${getComboString("reject_action")} or N)`}
					>
						Reject <kbd>{getComboString("reject_action")}</kbd>
					</button>
				</div>
			</div>
		{/if}
	</div>

	<!-- Footer. Hidden during an approval (the user is deciding, not chatting).
	     - streaming        → Stop button
	     - full agent chat  → the follow-up reply box -->
	{#if !approval}
		<div class="ai-footer">
			{#if streaming}
				<button class="stop-btn" onclick={() => onstop?.()}>■ Stop</button>
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

	{#if tokensOut > 0}
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

	/* An error is a message to the user, not a log line: sentence font, an icon
	   to anchor it, and a tinted panel so it reads as a state rather than as
	   failed output. */
	.ai-error {
		display: flex;
		align-items: flex-start;
		gap: 8px;
		padding: 10px 12px;
		border-radius: 8px;
		background: color-mix(in srgb, var(--error) 10%, transparent);
		border: 1px solid color-mix(in srgb, var(--error) 30%, transparent);
		color: var(--fg);
		font-size: 13px;
		font-family: var(--font-sans);
		line-height: 1.5;
	}

	.ai-error :global(.ai-error-icon) {
		flex-shrink: 0;
		margin-top: 1px;
		color: var(--error);
	}

	/* Answer actions: quiet until hovered, so they don't compete with the answer. */
	.answer-actions {
		display: flex;
		gap: 6px;
		margin-top: 8px;
	}

	.answer-action {
		display: flex;
		align-items: center;
		gap: 5px;
		padding: 4px 8px;
		background: none;
		border: 1px solid var(--border);
		border-radius: 6px;
		color: var(--fg-muted);
		font-family: var(--font-sans);
		font-size: 11px;
		cursor: pointer;
		opacity: 0.65;
		transition: opacity 120ms ease;
	}

	.answer-action:hover {
		opacity: 1;
		background: var(--bg-secondary);
		color: var(--fg);
	}

	/* The path "Open folder" action leads — it's the likely next step, and Enter
	   is bound to it. Accent-tinted and full-opacity so it reads as primary. */
	.answer-action-primary {
		opacity: 1;
		border-color: color-mix(in srgb, var(--accent-blue) 45%, var(--border));
		color: var(--accent-blue);
	}
	.answer-action-primary:hover {
		background: color-mix(in srgb, var(--accent-blue) 12%, transparent);
		color: var(--accent-blue);
	}
	.answer-action-primary kbd {
		font-family: var(--font-mono);
		font-size: 10px;
		line-height: 1;
		padding: 1px 4px;
		border-radius: 3px;
		border: 1px solid color-mix(in srgb, var(--accent-blue) 35%, var(--border));
		color: var(--accent-blue);
	}

	/* Code blocks get a copy affordance in the top-right corner (handled by a
	   delegated click, since the markdown is injected HTML). The label is CSS-only
	   so no interactive markup is inserted into sanitized output. */



	.truncated-note {
		margin-top: 10px;
		padding: 7px 10px;
		border-radius: 6px;
		border: 1px solid color-mix(in srgb, var(--warning-muted) 40%, var(--border));
		background: color-mix(in srgb, var(--warning-muted) 10%, transparent);
		color: var(--warning-muted);
		font-size: 12px;
		line-height: 1.45;
	}

	/* "Resumed your last run" cue — a slim bar at the top of a preserved
	   conversation. Accent-tinted (informational, not a warning) with the one
	   escape hatch on the right. */
	.resumed-banner {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 10px;
		margin-bottom: 8px;
		padding: 6px 10px;
		border-radius: 6px;
		border: 1px solid color-mix(in srgb, var(--accent-blue) 35%, var(--border));
		background: color-mix(in srgb, var(--accent-blue) 10%, transparent);
	}
	.resumed-label {
		font-size: 12px;
		color: var(--accent-blue);
	}
	.resumed-fresh {
		flex-shrink: 0;
		padding: 3px 9px;
		background: none;
		border: 1px solid color-mix(in srgb, var(--accent-blue) 35%, var(--border));
		border-radius: 5px;
		color: var(--accent-blue);
		font-family: var(--font-sans);
		font-size: 11px;
		cursor: pointer;
		opacity: 0.85;
		transition: opacity 120ms ease;
	}
	.resumed-fresh:hover {
		opacity: 1;
		background: color-mix(in srgb, var(--accent-blue) 14%, transparent);
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
	/* A large payload folded out of a user message into a collapsed chip. */
	/* Files attached to a turn — right-aligned to sit under the user bubble. */
	.turn-files {
		display: flex;
		flex-wrap: wrap;
		justify-content: flex-end;
		gap: 5px;
		margin: 0 0 8px auto;
		max-width: 85%;
	}
	.turn-file {
		display: flex;
		align-items: center;
		gap: 5px;
		max-width: 180px;
		padding: 3px 8px;
		background: var(--bg-secondary);
		border: 1px solid var(--border);
		border-radius: 6px;
		font-size: 11px;
		color: var(--fg-muted);
	}
	.turn-file-thumb {
		width: 16px;
		height: 16px;
		object-fit: cover;
		border-radius: 3px;
		flex-shrink: 0;
	}
	.turn-file-icon {
		font-size: 11px;
		flex-shrink: 0;
	}
	.turn-file-name {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.attachment {
		margin: 0 0 8px auto;
		max-width: 85%;
		width: fit-content;
	}
	.attachment-head {
		display: flex;
		align-items: center;
		gap: 6px;
		padding: 4px 9px;
		font-size: 12px;
		color: var(--fg-muted);
		background: var(--bg-secondary);
		border: 1px solid var(--border, rgba(128, 128, 128, 0.18));
		border-radius: 8px;
		cursor: pointer;
		font-family: inherit;
		transition: color 0.12s ease;
	}
	.attachment-head:hover {
		color: var(--fg);
	}
	.attachment-icon {
		font-size: 11px;
		opacity: 0.8;
	}
	.attachment-label {
		white-space: nowrap;
	}
	.attachment-caret {
		font-size: 9px;
		opacity: 0.7;
	}
	.attachment-body {
		margin-top: 4px;
		padding: 8px 10px;
		max-height: 240px;
		overflow-y: auto;
		font-size: 12px;
		line-height: 1.5;
		color: var(--fg-muted);
		white-space: pre-wrap;
		word-break: break-word;
		background: var(--bg-secondary);
		border-radius: 8px;
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

	/* The rendered markdown. `:global` because the HTML is injected, so scoped
	   selectors wouldn't reach it. Scoped under .ai-md to avoid leaking. */
	/* Language chip in the corner of a code block (from `data-lang`). */

	/* Tool-call steps — a compact row per tool the agent ran. */
	.tool-steps {
		display: flex;
		flex-direction: column;
		gap: 6px;
		margin-bottom: 10px;
	}
	/* Inline rich tool output — e.g. a QR code. White plate so a dark-theme
	   QR stays scannable; capped size, centered. */
	.tool-artifact.svg {
		align-self: flex-start;
		margin: 6px 0 4px;
		padding: 10px;
		background: var(--plate);
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
		flex-direction: column;
		font-family: var(--font-mono);
		font-size: 12px;
		color: var(--fg-muted);
	}
	.tool-step.failed {
		color: var(--error);
	}
	/* The clickable header row. A bare button reset — it must read as the
	   plain step row it replaced, gaining affordance only when expandable. */
	.tool-step-head {
		display: flex;
		align-items: center;
		gap: 7px;
		width: 100%;
		padding: 3px 6px;
		margin: 0 -6px;
		background: none;
		border: none;
		border-radius: 5px;
		font: inherit;
		color: inherit;
		text-align: left;
		cursor: default;
		transition: background 0.12s ease;
	}
	.tool-step-head.expandable {
		cursor: pointer;
	}
	.tool-step-head.expandable:hover {
		background: color-mix(in srgb, var(--fg) 6%, transparent);
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
		flex: 1;
	}
	.tool-caret {
		flex-shrink: 0;
		color: var(--fg-muted);
		font-size: 10px;
	}
	/* The revealed captured output — exactly what the tool returned to the
	   model. Scrolls on both axes so a long or wide result never blows out
	   the bubble; capped height so a huge dump stays a pane, not a wall. */
	.tool-output {
		margin: 6px 0 2px 21px;
		padding: 9px 11px;
		max-height: 280px;
		overflow: auto;
		background: var(--bg-secondary);
		border: 1px solid var(--border);
		border-left: 2px solid var(--accent-blue);
		border-radius: 6px;
		font-family: var(--font-mono);
		font-size: 11.5px;
		line-height: 1.55;
		color: var(--fg);
		white-space: pre-wrap;
		word-break: break-word;
		tab-size: 2;
	}
	.tool-output.live {
		animation: tool-output-pulse 1.4s ease-in-out infinite;
	}
	@keyframes tool-output-pulse {
		0%,
		100% {
			border-left-color: var(--accent-blue);
		}
		50% {
			border-left-color: color-mix(in srgb, var(--accent-blue) 35%, transparent);
		}
	}

	/* The whole line inherits its hue from the inline `style:color` (a per-quip
	   accent from the app's palette, theme-tuned in both light and dark). Full
	   strength — no muted colour or dimming — and a slow opacity pulse so the
	   line reads as "alive" while the model works. */
	.thinking {
		min-height: 1em;
		display: flex;
		align-items: center;
		gap: 5px;
		font-size: 9px;
		animation: thinking-pulse 1.6s ease-in-out infinite;
	}
	@keyframes thinking-pulse {
		0%,
		100% {
			opacity: 1;
		}
		50% {
			opacity: 0.55;
		}
	}
	/* The morphing ASCII face: monospace so the glyphs stay aligned. Colour is
	   inherited from `.thinking` (the per-quip accent), with a springy bob on
	   each swap (the {#key} in the template re-mounts it, replaying it). */
	.thinking-face {
		font-family: var(--font-mono);
		color: inherit;
		flex-shrink: 0;
		white-space: nowrap;
		font-size: 9px;
		animation: face-bob 0.45s cubic-bezier(0.34, 1.56, 0.64, 1);
	}
	@keyframes face-bob {
		0% {
			opacity: 0;
			transform: translateY(-3px) scale(0.9);
		}
		60% {
			transform: translateY(1px) scale(1.04);
		}
		100% {
			opacity: 1;
			transform: translateY(0) scale(1);
		}
	}
	/* The quip fades in alongside the face. */
	.thinking-verb {
		animation: verb-fade-in 0.45s ease;
	}
	@keyframes verb-fade-in {
		from {
			opacity: 0;
		}
		to {
			opacity: 1;
		}
	}
	/* The trailing ellipsis breathes so there is always motion even between
	   verb swaps (2.5s is a long time to hold still). */
	.thinking-ellipsis {
		animation: ellipsis-breathe 1.4s ease-in-out infinite;
	}
	@keyframes ellipsis-breathe {
		0%,
		100% {
			opacity: 0.3;
		}
		50% {
			opacity: 0.8;
		}
	}

	/* Destructive-tool approval prompt. */
	.approval {
		margin-top: 12px;
		padding: 10px 12px;
		border: 1px solid color-mix(in srgb, var(--warning-muted) 30%, transparent);
		background: color-mix(in srgb, var(--warning-muted) 8%, transparent);
		border-radius: 6px;
	}
	.approval-reason {
		color: var(--warning-muted);
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

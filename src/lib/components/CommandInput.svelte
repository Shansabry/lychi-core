<script lang="ts">
import { LoaderCircle } from "lucide-svelte";
import { matchesAction, normalizeKey } from "$lib/keybindings";

let {
	value = $bindable(""),
	onsubmit,
	onarrowup,
	onarrowdown,
	ondismiss,
	oninputchange,
	ontogglehistory,
	ontogglemedia,
	ontogglesettings,
	ontogglenotes,
	disabled = false,
	routing = false,
	executing = false,
	atMode = false,
	atStart = -1,
	searchMode = false,
	aiEnabled = false,
	scopeCount = 0,
	ontabscope = () => {},
	ontabcomplete = () => {},
	onshifttabback = () => {},
	ondrillinto = () => {},
	oncopypath = () => {},
	onactionpanel = () => {},
	onattachfile = () => {},
	onpastefiles = async () => false,
	onremovelastattachment = () => {},
	hasAttachments = false,
	contextPill = "",
	contextLoading = false,
	searchGhost = "",
	browseGhost = "",
	history = [],
}: {
	value: string;
	onsubmit: (opts?: { ctrlKey?: boolean; runInline?: boolean }) => void;
	onarrowup: () => void;
	onarrowdown: () => void;
	ondismiss: () => void;
	oninputchange: (value: string) => void;
	ontogglehistory: () => void;
	ontogglemedia: () => void;
	ontogglesettings: () => void;
	ontogglenotes: () => void;
	disabled: boolean;
	routing: boolean;
	executing: boolean;
	atMode: boolean;
	atStart: number;
	searchMode?: boolean;
	aiEnabled?: boolean;
	scopeCount?: number;
	ontabscope?: () => void;
	ontabcomplete?: () => void;
	onshifttabback?: () => void;
	ondrillinto?: () => void;
	oncopypath?: () => void;
	onactionpanel?: () => void;
	onattachfile?: () => void;
	onpastefiles?: () => Promise<boolean>;
	onremovelastattachment?: () => void;
	hasAttachments?: boolean;
	contextPill?: string;
	contextLoading?: boolean;
	searchGhost?: string;
	browseGhost?: string;
	history: string[];
} = $props();

// Split the input value into segments for the highlight overlay
let segments = $derived.by(() => {
	if (!atMode || atStart < 0) return null;
	const before = value.slice(0, atStart);
	const afterAt = value.slice(atStart);
	// Find end of @ reference (first space after @, or end of string)
	const spaceIdx = afterAt.indexOf(" ", 1);
	const atPart = spaceIdx === -1 ? afterAt : afterAt.slice(0, spaceIdx);
	const after = spaceIdx === -1 ? "" : afterAt.slice(spaceIdx);
	return { before, atPart, after };
});

// Ghost autofill from history — prefix match first, fuzzy fallback
type Ghost =
	| { kind: "suffix"; suffix: string }
	| { kind: "fuzzy"; full: string }
	| { kind: "at-suffix"; suffix: string }
	| null;

let ghost: Ghost = $derived.by(() => {
	if (!value || executing || routing) return null;
	// In browse mode, ghost shows suffix to complete the highlighted path
	if (atMode && browseGhost && atStart >= 0) {
		const partial = value.slice(atStart + 1); // text after @
		const ghostLabel = browseGhost.startsWith("~/") ? browseGhost.slice(2) : browseGhost;
		if (
			ghostLabel.toLowerCase().startsWith(partial.toLowerCase()) &&
			ghostLabel.length > partial.length
		) {
			return { kind: "at-suffix", suffix: ghostLabel.slice(partial.length) };
		}
		return null;
	}
	if (atMode) return null;
	// In search mode, ghost shows suffix to complete the highlighted path
	if (searchMode && searchGhost) {
		const ghostPath = searchGhost.startsWith("~/") ? searchGhost.slice(2) : searchGhost;
		const currentQuery = value.slice(1); // strip leading /
		if (ghostPath.toLowerCase().startsWith(currentQuery.toLowerCase())) {
			const suffix = ghostPath.slice(currentQuery.length);
			if (suffix) return { kind: "suffix", suffix };
		}
		return { kind: "fuzzy", full: searchGhost };
	}
	if (searchMode) return null;
	const lower = value.toLowerCase();
	// Ghost/inline autocomplete may ONLY show a literal prefix-extension of what
	// the user typed (browser-omnibox rule). A fuzzy, non-prefix match (e.g.
	// "run top" -> "run htop") must NOT appear as ghost — accepting it would run
	// something the user didn't type. Prefix matches only, most recent first.
	for (let i = history.length - 1; i >= 0; i--) {
		if (history[i].toLowerCase().startsWith(lower) && history[i] !== value) {
			return { kind: "suffix", suffix: history[i].slice(value.length) };
		}
	}
	return null;
});

let ghostSuffix = $derived(ghost?.kind === "suffix" ? ghost.suffix : "");
let ghostFull = $derived(ghost?.kind === "fuzzy" ? ghost.full : "");
let ghostAtSuffix = $derived(ghost?.kind === "at-suffix" ? ghost.suffix : "");

let inputEl: HTMLInputElement | undefined = $state();

$effect(() => {
	if (inputEl && !disabled) {
		inputEl.focus();
	}
});

// Animated placeholder suggestions. Split so we never advertise a capability
// the current setup can't deliver:
//   • BASE — deterministic commands that ALWAYS work (no AI, real handlers).
//     Each maps to a real trigger keyword and works with zero setup.
//   • AI_ONLY — phrases that need an AI provider; without one they'd silently
//     become a web search, so they only show when AI is on. The `<paste text>`
//     hint reflects the real mechanic: these operate on pasted/clipboard text,
//     NOT on any file or note Lychi goes and fetches for you.
const BASE_SUGGESTIONS = [
	"open firefox",
	"weather tokyo",
	"=256 * 1024",
	"convert 100 USD to EUR",
	"define ephemeral",
	"spotify next",
	"emoji rocket",
	"qr lychi.app",
	"color #3b82f6",
	"timer 5m",
	"reminder standup in 10m",
	"note buy milk",
	"bm docs",
	"ssh myserver",
	"screenshot area",
	"open @report",
	"github.com",
];
const AI_ONLY_SUGGESTIONS = [
	"what's the weather today?",
	"translate <paste text> to spanish",
	"summarize <paste text>",
	"rewrite to formal <paste text>",
];
// The active list depends on whether AI is configured.
let SUGGESTIONS = $derived(
	aiEnabled ? [...BASE_SUGGESTIONS, ...AI_ONLY_SUGGESTIONS] : BASE_SUGGESTIONS,
);

// Fisher-Yates shuffle
function shuffle<T>(arr: T[]): T[] {
	const a = [...arr];
	for (let i = a.length - 1; i > 0; i--) {
		const j = Math.floor(Math.random() * (i + 1));
		[a[i], a[j]] = [a[j], a[i]];
	}
	return a;
}

let placeholderText = $state("");
let showPlaceholder = $derived(value === "" && !executing && !routing);

// Run the typing animation loop as a side effect — fully timer-driven
$effect(() => {
	if (!showPlaceholder) {
		placeholderText = "";
		return;
	}

	let queue = shuffle(SUGGESTIONS);
	let pos = 0;
	let phase: "typing" | "paused" | "erasing" = "typing";
	let timer: ReturnType<typeof setTimeout>;
	let cancelled = false;

	function tick() {
		if (cancelled) return;
		const suggestion = queue[0];

		if (phase === "typing") {
			if (pos <= suggestion.length) {
				placeholderText = suggestion.slice(0, pos);
				pos++;
				timer = setTimeout(tick, 40 + Math.random() * 30);
			} else {
				phase = "paused";
				timer = setTimeout(tick, 2000);
			}
		} else if (phase === "paused") {
			phase = "erasing";
			tick();
		} else if (phase === "erasing") {
			if (pos > 0) {
				pos--;
				placeholderText = suggestion.slice(0, pos);
				timer = setTimeout(tick, 20);
			} else {
				placeholderText = "";
				// Advance — reshuffle when exhausted, ensuring no repeat
				const last = queue.shift() as string;
				if (queue.length === 0) {
					queue = shuffle(SUGGESTIONS);
					if (queue[0] === last) {
						queue.push(queue.shift() as string);
					}
				}
				phase = "typing";
				timer = setTimeout(tick, 400);
			}
		}
	}

	timer = setTimeout(tick, 600); // Initial delay before first suggestion

	return () => {
		cancelled = true;
		clearTimeout(timer);
	};
});

function acceptGhost() {
	if (searchMode && searchGhost) {
		// Fill highlighted result into input as a /path query
		const filled = searchGhost.startsWith("~/") ? `/${searchGhost.slice(2)}` : `/${searchGhost}`;
		value = filled;
		oninputchange(value);
		return true;
	}
	if (ghost?.kind === "at-suffix") {
		value = value + ghost.suffix;
		oninputchange(value);
		return true;
	}
	if (ghost?.kind === "suffix") {
		value = value + ghost.suffix;
		oninputchange(value);
		return true;
	}
	if (ghost?.kind === "fuzzy") {
		value = ghost.full;
		oninputchange(value);
		return true;
	}
	return false;
}

/**
 * Ctrl+V — stage copied files/images as attachments instead of pasting text.
 *
 * We can't know from the event whether the clipboard holds files, so we let the
 * browser's own text paste happen normally and ask the backend in parallel. If
 * it staged something, the clipboard held FILES (never text — the backend
 * ignores text), and any text the browser inserted is spurious, so we undo it by
 * restoring the pre-paste value.
 */
function handlePaste(e: ClipboardEvent) {
	// A text/plain payload is unambiguously a text paste — leave it alone. Only
	// probe the backend when the clipboard offers no text (a file/image copy).
	if (e.clipboardData?.types.includes("text/plain")) return;
	const before = value;
	onpastefiles().then((staged) => {
		if (staged && value !== before) {
			value = before;
			oninputchange(value);
		}
	});
}

function handleKeydown(e: KeyboardEvent) {
	if (matchesAction(e, "tab_back")) {
		e.preventDefault();
		onshifttabback();
	} else if (matchesAction(e, "switch_scope") && scopeCount > 1) {
		e.preventDefault();
		ontabscope();
	} else if (matchesAction(e, "tab_complete") && (searchMode || atMode)) {
		e.preventDefault();
		ontabcomplete();
	} else if (matchesAction(e, "tab_complete") && ghost) {
		e.preventDefault();
		acceptGhost();
	} else if (
		e.key === "ArrowRight" &&
		ghost &&
		inputEl &&
		inputEl.selectionStart === value.length
	) {
		e.preventDefault();
		acceptGhost();
	} else if (
		e.key === "ArrowRight" &&
		searchMode &&
		inputEl &&
		inputEl.selectionStart === value.length
	) {
		// Search mode, cursor at end, no ghost → drill into the selected folder.
		e.preventDefault();
		ondrillinto();
	} else if (matchesAction(e, "copy_path") && searchMode) {
		e.preventDefault();
		oncopypath();
	} else if (matchesAction(e, "attach_file")) {
		// Ctrl+Shift+A — stage the highlighted file for the next AI turn.
		e.preventDefault();
		onattachfile();
	} else if (e.key === "Backspace" && value === "" && hasAttachments) {
		// Backspace on an empty input pops the last chip — the universal
		// composer gesture (Slack/Discord/Gmail).
		e.preventDefault();
		onremovelastattachment();
	} else if (matchesAction(e, "action_panel")) {
		// ⌘K / Ctrl+K — open the secondary-actions panel for the selected result.
		e.preventDefault();
		onactionpanel();
	} else if (matchesAction(e, "web_search")) {
		e.preventDefault();
		onsubmit({ ctrlKey: true });
	} else if (matchesAction(e, "run_inline")) {
		e.preventDefault();
		onsubmit({ runInline: true });
	} else if (matchesAction(e, "submit") && !e.shiftKey) {
		e.preventDefault();
		onsubmit({ ctrlKey: false });
	} else if (e.key === "ArrowUp") {
		e.preventDefault();
		onarrowup();
	} else if (e.key === "ArrowDown") {
		e.preventDefault();
		onarrowdown();
	} else if (matchesAction(e, "dismiss")) {
		e.preventDefault();
		ondismiss();
	} else if (matchesAction(e, "toggle_history")) {
		e.preventDefault();
		ontogglehistory();
	} else if (matchesAction(e, "toggle_notes")) {
		e.preventDefault();
		ontogglenotes();
	} else if (matchesAction(e, "toggle_media")) {
		e.preventDefault();
		ontogglemedia();
	} else if (matchesAction(e, "toggle_settings")) {
		e.preventDefault();
		ontogglesettings();
	}
}
</script>

<div class="input-container">
	<span class="prompt" class:routing class:executing class:hidden-prompt={!executing && /^[>@/=]/.test(value)}>
		{#if executing}
			<LoaderCircle size={18} strokeWidth={1.5} />
		{:else}
			&gt;
		{/if}
	</span>
	<div class="input-wrapper">
		{#if segments}
			<div class="highlight-overlay" aria-hidden="true">
				<span class="hl-text">{segments.before}</span><span class="hl-at">{segments.atPart}</span><span class="hl-text">{segments.after}</span>
			</div>
		{/if}
		{#if showPlaceholder}
			<div class="placeholder-overlay" aria-hidden="true">
				<span class="placeholder-text">{placeholderText}</span>
			</div>
		{/if}
		<div class="ghost-overlay" aria-hidden="true" style:visibility={ghostSuffix ? "visible" : "hidden"}>
			<span class="ghost-typed">{value}</span><span class="ghost-suffix">{ghostSuffix}</span>
		</div>
		<div class="ghost-overlay" aria-hidden="true" style:visibility={ghostFull ? "visible" : "hidden"}>
			<span class="ghost-typed">{value}</span><span class="ghost-fuzzy-hint"> ~{ghostFull}</span>
		</div>
		<div class="ghost-overlay" aria-hidden="true" style:visibility={ghostAtSuffix ? "visible" : "hidden"}>
			<span class="ghost-typed">{value}</span><span class="ghost-suffix">{ghostAtSuffix}</span>
		</div>
		<!-- svelte-ignore a11y_autofocus — launcher input must grab focus immediately -->
		<input
			bind:this={inputEl}
			bind:value
			onkeydown={handleKeydown}
			onpaste={handlePaste}
			oninput={(e) => oninputchange(e.currentTarget.value)}
			disabled={disabled || routing}
			type="text"
			spellcheck="false"
			autocomplete="off"
			autofocus
		/>
	</div>
	{#if contextLoading && !contextPill}
		<span class="context-pill context-pill-loading">
			<LoaderCircle size={10} strokeWidth={1.5} />
		</span>
	{:else if contextPill}
		<span class="context-pill">{contextPill}</span>
	{/if}
</div>

<style>
	.input-container {
		display: flex;
		align-items: center;
		gap: 12px;
		padding: 16px 20px;
		background: var(--bg);
		border-bottom: none;
		-webkit-app-region: drag;
	}

	.prompt {
		color: var(--fg-muted);
		font-family: var(--font-mono);
		font-size: 18px;
		user-select: none;
		display: flex;
		align-items: center;
		gap: 6px;
		height: 18px;
		transition: color 200ms ease;
		white-space: nowrap;
	}

	.context-pill {
		flex-shrink: 0;
		font-size: 10px;
		font-family: var(--font-mono);
		color: var(--fg-muted);
		opacity: 0.45;
		background: rgba(255, 255, 255, 0.06);
		padding: 2px 8px;
		border-radius: 9999px;
		white-space: nowrap;
		user-select: none;
	}

	.context-pill-loading {
		display: flex;
		align-items: center;
		opacity: 0.3;
		padding: 3px 8px;
	}

	.context-pill-loading :global(svg) {
		animation: pill-spin 700ms linear infinite;
	}

	@keyframes pill-spin {
		from { transform: rotate(0deg); }
		to { transform: rotate(360deg); }
	}

	@media (prefers-reduced-motion: reduce) {
		.context-pill-loading :global(svg) { animation: none; }
	}

	.prompt.hidden-prompt {
		display: none;
	}

	.prompt.routing {
		color: var(--accent);
		animation: prompt-pulse 800ms ease-in-out infinite;
	}

	.prompt.executing {
		color: var(--fg-muted);
	}

	.prompt.executing :global(svg) {
		animation: prompt-spin 700ms linear infinite;
	}

	@keyframes prompt-pulse {
		0%,
		100% {
			opacity: 0.4;
		}
		50% {
			opacity: 1;
		}
	}

	@keyframes prompt-spin {
		from {
			transform: rotate(0deg);
		}
		to {
			transform: rotate(360deg);
		}
	}

	.input-wrapper {
		flex: 1;
		position: relative;
		overflow: hidden;
	}

	.highlight-overlay {
		position: absolute;
		top: 0;
		left: 0;
		right: 0;
		bottom: 0;
		pointer-events: none;
		font-family: var(--font-mono);
		font-size: 18px;
		line-height: normal;
		white-space: pre;
		display: flex;
		align-items: center;
	}

	.hl-text {
		color: transparent;
	}

	.hl-at {
		color: transparent;
		background: rgba(0, 255, 200, 0.12);
		border-radius: 3px;
		padding: 1px 0;
	}

	.placeholder-overlay {
		position: absolute;
		top: 0;
		left: 0;
		right: 0;
		bottom: 0;
		pointer-events: none;
		font-family: var(--font-mono);
		font-size: 18px;
		line-height: normal;
		white-space: pre;
		display: flex;
		align-items: center;
	}

	.placeholder-text {
		color: var(--fg-muted);
		opacity: 0.5;
	}

	.ghost-overlay {
		position: absolute;
		top: 0;
		left: 0;
		right: 0;
		bottom: 0;
		pointer-events: none;
		font-family: var(--font-mono);
		font-size: 18px;
		line-height: normal;
		white-space: pre;
		display: flex;
		align-items: center;
		will-change: transform;
	}

	.ghost-typed {
		color: transparent;
	}

	.ghost-suffix {
		color: var(--fg-muted);
		opacity: 0.35;
	}

	.ghost-fuzzy-hint {
		color: var(--fg-muted);
		opacity: 0.25;
		font-style: italic;
	}

	input {
		width: 100%;
		background: none;
		border: none;
		outline: none;
		color: var(--accent);
		font-family: var(--font-mono);
		font-size: 18px;
		caret-color: var(--accent);
		-webkit-app-region: no-drag;
		position: relative;
	}

	input:disabled {
		opacity: 0.5;
	}
</style>

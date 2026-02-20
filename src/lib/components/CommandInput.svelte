<script lang="ts">
import { LoaderCircle } from "lucide-svelte";
import { fuzzyRank } from "$lib/fuzzy";
import { matchesAction } from "$lib/keybindings";

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
	scopeCount = 0,
	ontabscope = () => {},
	ontabcomplete = () => {},
	onshifttabback = () => {},
	searchGhost = "",
	browseGhost = "",
	history = [],
}: {
	value: string;
	onsubmit: () => void;
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
	scopeCount?: number;
	ontabscope?: () => void;
	ontabcomplete?: () => void;
	onshifttabback?: () => void;
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
	// Prefix match wins (most recent first)
	for (let i = history.length - 1; i >= 0; i--) {
		if (history[i].toLowerCase().startsWith(lower) && history[i] !== value) {
			return { kind: "suffix", suffix: history[i].slice(value.length) };
		}
	}
	// Fuzzy fallback — only for meaningful queries
	if (value.length >= 3) {
		const matches = fuzzyRank(value, history);
		if (matches.length > 0 && matches[0].value !== value) {
			return { kind: "fuzzy", full: matches[0].value };
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

// Animated placeholder suggestions
const SUGGESTIONS = [
	"open firefox",
	"web how to make pasta",
	"yt lofi hip hop",
	"=256 * 1024",
	">ls -la ~/Downloads",
	"resize @~/Photos/img.jpg to 800x600",
	"spotify next",
	"what's the weather today?",
	"summarize my last meeting notes",
	"convert 100 USD to EUR",
	"create a new project folder",
	"find large files in Downloads",
	"pause all music",
	"github.com",
];

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
	} else if (matchesAction(e, "submit") && !e.shiftKey) {
		e.preventDefault();
		onsubmit();
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
	<span class="prompt" class:routing class:executing>
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
			oninput={(e) => oninputchange(e.currentTarget.value)}
			disabled={disabled || routing}
			type="text"
			spellcheck="false"
			autocomplete="off"
			autofocus
		/>
	</div>
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
		justify-content: center;
		width: 18px;
		height: 18px;
		transition: color 200ms ease;
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

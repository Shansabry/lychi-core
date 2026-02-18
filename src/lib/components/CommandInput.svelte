<script lang="ts">
import { LoaderCircle } from "lucide-svelte";

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
	disabled = false,
	routing = false,
	executing = false,
	atMode = false,
	atStart = -1,
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
	disabled: boolean;
	routing: boolean;
	executing: boolean;
	atMode: boolean;
	atStart: number;
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

// Ghost autofill from history — find the most recent match for current input
let ghostSuffix = $derived.by(() => {
	if (!value || executing || routing) return "";
	const lower = value.toLowerCase();
	// Search history in reverse (most recent first)
	for (let i = history.length - 1; i >= 0; i--) {
		if (history[i].toLowerCase().startsWith(lower) && history[i] !== value) {
			return history[i].slice(value.length);
		}
	}
	return "";
});

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
	if (ghostSuffix) {
		value = value + ghostSuffix;
		oninputchange(value);
		return true;
	}
	return false;
}

function handleKeydown(e: KeyboardEvent) {
	if (e.key === "Tab" && ghostSuffix) {
		e.preventDefault();
		acceptGhost();
	} else if (
		e.key === "ArrowRight" &&
		ghostSuffix &&
		inputEl &&
		inputEl.selectionStart === value.length
	) {
		e.preventDefault();
		acceptGhost();
	} else if (e.key === "Enter" && !e.shiftKey) {
		e.preventDefault();
		onsubmit();
	} else if (e.key === "ArrowUp") {
		e.preventDefault();
		onarrowup();
	} else if (e.key === "ArrowDown") {
		e.preventDefault();
		onarrowdown();
	} else if (e.key === "Escape") {
		e.preventDefault();
		ondismiss();
	} else if (e.ctrlKey && e.key === "1") {
		e.preventDefault();
		ontogglehistory();
	} else if (e.ctrlKey && e.key === "2") {
		e.preventDefault();
		ontogglemedia();
	} else if (e.ctrlKey && e.key === "3") {
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
		{#if ghostSuffix}
			<div class="ghost-overlay" aria-hidden="true">
				<span class="ghost-typed">{value}</span><span class="ghost-suffix">{ghostSuffix}</span>
			</div>
		{/if}
		<input
			bind:this={inputEl}
			bind:value
			onkeydown={handleKeydown}
			oninput={(e) => oninputchange(e.currentTarget.value)}
			disabled={disabled || routing}
			type="text"
			spellcheck="false"
			autocomplete="off"
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
	}

	.ghost-typed {
		color: transparent;
	}

	.ghost-suffix {
		color: var(--fg-muted);
		opacity: 0.35;
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

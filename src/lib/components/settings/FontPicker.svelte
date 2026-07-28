<script lang="ts">
import type { FontFamily } from "$lib/ipc";

/**
 * Font picker: every option is rendered in the typeface it names.
 *
 * Three decisions, each from a real failure:
 *
 * 1. **Every row renders in its own font.** A list of names in the UI font
 *    tells you nothing about what you're choosing — the universal font-picker
 *    convention exists because seeing the face IS the information.
 *
 * 2. **Nothing is applied until you commit.** Applying each highlighted font to
 *    the whole app was tried and removed: repainting every surface per keystroke
 *    is visibly slow in this WebView (see the first-render notes in
 *    core/CLAUDE.md), and the flashing made browsing harder, not easier. The
 *    per-row previews already show the typeface; the commit shows the layout.
 *
 * 3. **Rows are windowed.** This machine reports ~1200 families. Building 1200
 *    rows — each forcing a distinct font to load — is exactly the WebView
 *    first-paint cost documented in core/CLAUDE.md. Only the visible slice is
 *    rendered.
 */

let {
	value = "",
	fonts = [] as FontFamily[],
	onchange,
}: {
	/** The committed family name, or "" for the built-in stack. */
	value: string;
	/** Installed families, from fontconfig via the backend. */
	fonts: FontFamily[];
	/** Commit a choice (persist + apply). */
	onchange: (value: string) => void;
} = $props();

/** Row height in px — fixed so the windowing maths stays simple. */
const ROW_H = 30;
/** Rows rendered beyond the viewport, so fast scrolling doesn't show gaps. */
const OVERSCAN = 4;
/** Dropdown viewport height. */
const LIST_H = 260;

let open = $state(false);
let query = $state("");
let scrollTop = $state(0);
let activeIndex = $state(0);
let buttonEl: HTMLButtonElement | undefined = $state();
let listEl: HTMLDivElement | undefined = $state();
let inputEl: HTMLInputElement | undefined = $state();
let dropdownStyle = $state("");

// "System default" is a real option, not a placeholder: it means "no override",
// which is different from picking whichever font happens to be first.
const DEFAULT_OPTION = { name: "", label: "System default", monospace: false };

let filtered = $derived.by(() => {
	const q = query.trim().toLowerCase();
	// Monospace families first: Lychi's interface is monospace-heavy by design,
	// so those are the likely picks.
	const sorted = [...fonts].sort((a, b) => {
		if (a.monospace !== b.monospace) return a.monospace ? -1 : 1;
		return a.name.localeCompare(b.name);
	});
	const matching = q ? sorted.filter((f) => f.name.toLowerCase().includes(q)) : sorted;
	const rows = matching.map((f) => ({ name: f.name, label: f.name, monospace: f.monospace }));
	// The default stays reachable at the top unless it's filtered out by name.
	return q && !DEFAULT_OPTION.label.toLowerCase().includes(q) ? rows : [DEFAULT_OPTION, ...rows];
});

// --- windowing ---
let firstVisible = $derived(Math.max(0, Math.floor(scrollTop / ROW_H) - OVERSCAN));
let visibleCount = $derived(Math.ceil(LIST_H / ROW_H) + OVERSCAN * 2);
let windowRows = $derived(filtered.slice(firstVisible, firstVisible + visibleCount));

let selectedLabel = $derived(value || DEFAULT_OPTION.label);

function positionDropdown() {
	if (!buttonEl) return;
	const rect = buttonEl.getBoundingClientRect();
	const spaceBelow = window.innerHeight - rect.bottom;
	const openUp = spaceBelow < LIST_H + 60;
	dropdownStyle = openUp
		? `position:fixed;left:${rect.left}px;bottom:${window.innerHeight - rect.top + 2}px;width:${Math.max(rect.width, 240)}px;`
		: `position:fixed;left:${rect.left}px;top:${rect.bottom + 2}px;width:${Math.max(rect.width, 240)}px;`;
}

function openList() {
	positionDropdown();
	open = true;
	query = "";
	scrollTop = 0;
	activeIndex = Math.max(
		0,
		filtered.findIndex((f) => f.name === value),
	);
	requestAnimationFrame(() => {
		inputEl?.focus();
		scrollActiveIntoView();
	});
}

/** Close without committing. Nothing was applied, so there is nothing to undo. */
function cancel() {
	open = false;
	buttonEl?.focus();
}

function commit(name: string) {
	open = false;
	onchange(name);
	buttonEl?.focus();
}

function scrollActiveIntoView() {
	if (!listEl) return;
	const top = activeIndex * ROW_H;
	const bottom = top + ROW_H;
	if (top < listEl.scrollTop) listEl.scrollTop = top;
	else if (bottom > listEl.scrollTop + LIST_H) listEl.scrollTop = bottom - LIST_H;
}

function move(step: number) {
	if (!filtered.length) return;
	activeIndex = Math.min(filtered.length - 1, Math.max(0, activeIndex + step));
	scrollActiveIntoView();
}

function handleKeydown(e: KeyboardEvent) {
	if (!open) {
		if (e.key === "Enter" || e.key === " " || e.key === "ArrowDown") {
			e.preventDefault();
			openList();
		}
		return;
	}
	if (e.key === "Escape") {
		e.preventDefault();
		e.stopPropagation();
		cancel();
		return;
	}
	if (e.key === "ArrowDown") {
		e.preventDefault();
		move(1);
		return;
	}
	if (e.key === "ArrowUp") {
		e.preventDefault();
		move(-1);
		return;
	}
	if (e.key === "Enter") {
		e.preventDefault();
		if (filtered[activeIndex]) commit(filtered[activeIndex].name);
	}
}

function handleBlur(e: FocusEvent) {
	const related = e.relatedTarget as Node | null;
	if (related && (listEl?.contains(related) || related === inputEl)) return;
	if (open) cancel();
}
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="font-picker" onkeydown={handleKeydown}>
	<button
		class="picker-trigger"
		class:open
		type="button"
		bind:this={buttonEl}
		onclick={() => (open ? cancel() : openList())}
		onblur={handleBlur}
		aria-haspopup="listbox"
		aria-expanded={open}
	>
		<!-- The trigger previews the CURRENT font, so the committed choice is
		     legible at a glance without opening the list. -->
		<span class="trigger-value" style:font-family={value ? `"${value}"` : "var(--font-sans)"}>
			{selectedLabel}
		</span>
		<svg class="chevron" width="10" height="6" viewBox="0 0 10 6" fill="none" aria-hidden="true">
			<path
				d="M1 1L5 5L9 1"
				stroke="currentColor"
				stroke-width="1.5"
				stroke-linecap="round"
				stroke-linejoin="round"
			/>
		</svg>
	</button>

	{#if open}
		<div class="dropdown" style={dropdownStyle}>
			<input
				class="filter"
				type="text"
				bind:this={inputEl}
				bind:value={query}
				placeholder="Filter fonts…"
				spellcheck="false"
				onblur={handleBlur}
				oninput={() => {
					activeIndex = 0;
					scrollTop = 0;
					if (listEl) listEl.scrollTop = 0;
				}}
			/>
			<div
				class="rows"
				style:height="{LIST_H}px"
				bind:this={listEl}
				onscroll={(e) => (scrollTop = e.currentTarget.scrollTop)}
				role="listbox"
				tabindex="-1"
			>
				<!-- A spacer of the full list height gives a real scrollbar while only
				     the visible slice exists in the DOM. -->
				<div class="spacer" style:height="{filtered.length * ROW_H}px">
					{#each windowRows as row, i (row.name || "__default__")}
						{@const idx = firstVisible + i}
						<button
							class="row"
							class:active={idx === activeIndex}
							class:selected={row.name === value}
							style:top="{idx * ROW_H}px"
							style:height="{ROW_H}px"
							type="button"
							role="option"
							aria-selected={row.name === value}
							onmouseenter={() => (activeIndex = idx)}
							onclick={() => commit(row.name)}
							onblur={handleBlur}
						>
							<!-- Rendered IN the font it names: the whole point of the picker. -->
							<span
								class="row-name"
								style:font-family={row.name ? `"${row.name}"` : "var(--font-sans)"}
							>
								{row.label}
							</span>
							{#if row.monospace}
								<span class="row-tag">mono</span>
							{/if}
						</button>
					{/each}
				</div>
			</div>
			<div class="hint">↑↓ browse · ↵ apply · esc cancel</div>
		</div>
	{/if}
</div>

<style>
	.font-picker {
		position: relative;
		flex: 1;
		min-width: 0;
	}

	.picker-trigger {
		width: 100%;
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 8px;
		background: var(--bg-secondary);
		color: var(--fg);
		border: 1px solid var(--border);
		border-radius: 4px;
		padding: 5px 8px;
		font-size: 12px;
		cursor: pointer;
		outline: none;
		text-align: left;
	}

	.picker-trigger:focus,
	.picker-trigger.open {
		border-color: var(--fg-muted);
	}

	.trigger-value {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.chevron {
		flex-shrink: 0;
		color: var(--fg-muted);
	}

	.dropdown {
		z-index: 1000;
		background: var(--bg-secondary);
		border: 1px solid var(--border);
		border-radius: 6px;
		box-shadow: 0 8px 24px var(--shadow-overlay);
		overflow: hidden;
	}

	.filter {
		width: 100%;
		background: transparent;
		color: var(--fg);
		border: none;
		border-bottom: 1px solid var(--border);
		padding: 7px 10px;
		font-family: var(--font-mono);
		font-size: 12px;
		outline: none;
	}

	.rows {
		overflow-y: auto;
		overflow-x: hidden;
	}

	/* Full-height spacer + absolutely positioned rows: the scrollbar reflects the
	   whole list while only the visible slice is in the DOM. */
	.spacer {
		position: relative;
	}

	.row {
		position: absolute;
		left: 0;
		right: 0;
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 8px;
		padding: 0 10px;
		background: none;
		border: none;
		color: var(--fg);
		font-size: 13px;
		cursor: pointer;
		text-align: left;
		overflow: hidden;
	}

	.row.active {
		background: var(--overlay-wash);
	}

	.row.selected .row-name {
		color: var(--accent);
	}

	.row-name {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.row-tag {
		flex-shrink: 0;
		font-family: var(--font-mono);
		font-size: 9px;
		color: var(--fg-muted);
		border: 1px solid var(--border);
		border-radius: 3px;
		padding: 0 4px;
	}

	.hint {
		border-top: 1px solid var(--border);
		padding: 5px 10px;
		font-family: var(--font-mono);
		font-size: 10px;
		color: var(--fg-muted);
		opacity: 0.7;
	}
</style>

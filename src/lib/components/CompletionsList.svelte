<script lang="ts">
import { convertFileSrc } from "@tauri-apps/api/core";
import { AppWindow, File, Folder, LoaderCircle } from "lucide-svelte";
import type { CompletionItem, MountPoint } from "$lib/ipc";

let {
	items,
	selectedIndex,
	onselect,
	pathContext = "",
	scopeTabs = [],
	activeScopeIndex = 0,
	onscopechange = () => {},
	searching = false,
	browseMode = false,
}: {
	items: CompletionItem[];
	selectedIndex: number;
	onselect: (label: string) => void;
	pathContext?: string;
	scopeTabs?: MountPoint[];
	activeScopeIndex?: number;
	onscopechange?: (index: number) => void;
	searching?: boolean;
	browseMode?: boolean;
} = $props();

let listEl: HTMLUListElement | undefined = $state();

// Count non-item rows above the list items for scroll offset
let headerRows = $derived((pathContext ? 1 : 0) + (scopeTabs.length > 1 ? 1 : 0));

// Scroll selected item into view when selection changes
$effect(() => {
	if (listEl && selectedIndex >= 0) {
		const item = listEl.children[selectedIndex + headerRows] as HTMLElement | undefined;
		item?.scrollIntoView({ block: "nearest" });
	}
});

function iconSrc(path: string | null): string | null {
	if (!path) return null;
	if ("__TAURI_INTERNALS__" in window) {
		return convertFileSrc(path);
	}
	return path;
}

function displayName(label: string): string {
	// Show just the filename for path labels (e.g. "~/Documents/foo.txt" → "foo.txt")
	const lastSlash = label.lastIndexOf("/");
	if (lastSlash === -1) return label;
	// For directories, strip trailing slash for display, then get last segment
	if (label.endsWith("/")) {
		const trimmed = label.slice(0, -1);
		const idx = trimmed.lastIndexOf("/");
		return idx === -1 ? `${trimmed}/` : `${trimmed.slice(idx + 1)}/`;
	}
	return label.slice(lastSlash + 1);
}

// In search mode, show filename + muted parent path
function searchDisplayName(label: string): { name: string; parent: string } {
	const lastSlash = label.lastIndexOf("/");
	if (lastSlash === -1) return { name: label, parent: "" };
	if (label.endsWith("/")) {
		const trimmed = label.slice(0, -1);
		const idx = trimmed.lastIndexOf("/");
		return {
			name: idx === -1 ? `${trimmed}/` : `${trimmed.slice(idx + 1)}/`,
			parent: idx === -1 ? "" : trimmed.slice(0, idx + 1),
		};
	}
	return {
		name: label.slice(lastSlash + 1),
		parent: label.slice(0, lastSlash + 1),
	};
}

let isSearchMode = $derived(scopeTabs.length > 0 || searching);
</script>

<ul class="completions" role="listbox" bind:this={listEl}>
	{#if scopeTabs.length > 1}
		<li class="scope-tabs" aria-hidden="true">
			{#each scopeTabs as tab, i}
				<button
					class="scope-tab"
					class:active={i === activeScopeIndex}
					tabindex={-1}
					onmousedown={(e) => e.preventDefault()}
					onclick={() => onscopechange(i)}
				>
					{tab.label}
				</button>
			{/each}
			<span class="search-indicator" class:visible={searching}>
				<LoaderCircle size={12} strokeWidth={1.5} />
			</span>
		</li>
	{:else if searching}
		<li class="scope-tabs" aria-hidden="true">
			<span class="search-indicator visible">
				<LoaderCircle size={12} strokeWidth={1.5} />
				<span class="search-label">Searching...</span>
			</span>
		</li>
	{/if}
	{#if pathContext}
		<li class="breadcrumb" aria-hidden="true">
			<span class="breadcrumb-path">{pathContext}</span>
		</li>
	{/if}
	{#each items as item, i (item.label)}
		<li
			class="completion-item"
			class:selected={i === selectedIndex}
			onmousedown={(e) => e.preventDefault()}
			onclick={() => onselect(item.label)}
			onkeydown={(e) => e.key === "Enter" && onselect(item.label)}
			role="option"
			aria-selected={i === selectedIndex}
			tabindex="-1"
		>
			<span class="icon">
				{#if item.icon_path === "__folder__"}
					<Folder size={20} strokeWidth={1.5} class="icon-folder" />
				{:else if item.icon_path}
					{@const src = iconSrc(item.icon_path)}
					{#if src}
						<img src={src} alt="" width="24" height="24" decoding="async" loading="eager" />
					{:else}
						<File size={20} strokeWidth={1.5} class="icon-file" />
					{/if}
				{:else}
					<AppWindow size={20} strokeWidth={1.5} class="icon-fallback" />
				{/if}
			</span>
			{#if isSearchMode}
				{@const display = searchDisplayName(item.label)}
				<div class="label-group">
					<span class="label">{display.name}</span>
					{#if display.parent}
						<span class="search-path">{display.parent}</span>
					{/if}
				</div>
			{:else}
				<div class="label-group">
					<span class="label">{pathContext ? displayName(item.label) : item.label}</span>
					{#if item.description}
						<span class="description">{item.description}</span>
					{/if}
				</div>
			{/if}
		</li>
	{/each}
	{#if items.length > 0}
		<li class="hints" aria-hidden="true">
			<span class="hint"><kbd>↑↓</kbd> navigate</span>
			<span class="hint"><kbd>↵</kbd> {isSearchMode ? "open" : (items[selectedIndex]?.icon_path === "__folder__" ? "open folder" : "select")}</span>
			{#if (browseMode || isSearchMode) && items[selectedIndex]?.icon_path === "__folder__"}
				<span class="hint"><kbd>tab</kbd> drill into</span>
			{/if}
			{#if isSearchMode}
				<span class="hint"><kbd>⇧tab</kbd> go back</span>
			{:else if browseMode && pathContext && pathContext !== "~/"}
				<span class="hint"><kbd>⇧tab</kbd> go back</span>
			{/if}
			{#if scopeTabs.length > 1}
				<span class="hint"><kbd>ctrl+tab</kbd> switch scope</span>
			{/if}
			<span class="hint"><kbd>esc</kbd> dismiss</span>
		</li>
	{/if}
</ul>

<style>
	.completions {
		list-style: none;
		overflow-y: auto;
		flex: 1;
		min-height: 0;
	}



	.scope-tabs {
		display: flex;
		align-items: center;
		gap: 4px;
		padding: 6px 20px 4px;
		user-select: none;
	}

	.scope-tab {
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--fg-muted);
		background: none;
		border: 1px solid var(--border);
		border-radius: 10px;
		padding: 2px 10px;
		cursor: pointer;
		transition: color 80ms ease, border-color 80ms ease;
	}

	.scope-tab:hover {
		color: var(--fg);
		border-color: var(--fg-muted);
	}

	.scope-tab.active {
		color: var(--accent);
		border-color: var(--accent);
	}

	.search-indicator {
		display: flex;
		align-items: center;
		gap: 6px;
		margin-left: auto;
		color: var(--fg-muted);
		opacity: 0;
		transition: opacity 150ms ease;
		pointer-events: none;
	}

	.search-indicator.visible {
		opacity: 0.5;
	}

	.search-indicator :global(svg) {
		animation: spin 700ms linear infinite;
	}

	@keyframes spin {
		from { transform: rotate(0deg); }
		to { transform: rotate(360deg); }
	}

	.search-label {
		font-family: var(--font-mono);
		font-size: 11px;
	}

	.breadcrumb {
		padding: 6px 20px 2px;
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--fg-muted);
		opacity: 0.6;
		user-select: none;
	}

	.completion-item {
		display: flex;
		align-items: center;
		gap: 12px;
		padding: 8px 20px 8px 18px;
		cursor: pointer;
		border-left: 2px solid transparent;
		transition: background 80ms ease, padding-left 80ms ease, border-color 80ms ease;
	}

	.completion-item:hover {
		background: var(--bg-secondary);
	}

	.completion-item.selected {
		background: var(--bg-secondary);
		border-left-color: var(--accent);
		padding-left: 20px;
	}

	.icon {
		width: 24px;
		height: 24px;
		display: flex;
		align-items: center;
		justify-content: center;
		flex-shrink: 0;
	}

	.icon img {
		width: 24px;
		height: 24px;
		object-fit: contain;
	}

	.icon :global(.icon-fallback) {
		color: var(--fg-muted);
		opacity: 0.5;
	}

	.icon :global(.icon-folder) {
		color: var(--accent);
	}

	.icon :global(.icon-file) {
		color: var(--fg-muted);
	}

	.label-group {
		display: flex;
		align-items: baseline;
		gap: 8px;
		min-width: 0;
		flex: 1;
	}

	.label {
		font-family: var(--font-mono);
		font-size: 14px;
		color: var(--fg);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	.description {
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--fg-muted);
		opacity: 0.5;
		flex-shrink: 0;
	}

	.search-path {
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--fg-muted);
		opacity: 0.4;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
		margin-left: auto;
	}

	.completion-item.selected .label {
		color: var(--accent);
	}

	.hints {
		display: flex;
		gap: 16px;
		padding: 4px 20px 6px;
		user-select: none;
	}

	.hint {
		font-family: var(--font-mono);
		font-size: 10px;
		color: var(--fg-muted);
		opacity: 0.4;
	}

	.hint kbd {
		font-family: inherit;
		font-size: inherit;
		opacity: 0.7;
	}
</style>

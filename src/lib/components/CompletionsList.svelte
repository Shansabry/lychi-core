<script lang="ts">
import { convertFileSrc } from "@tauri-apps/api/core";
import { AppWindow, Folder, LoaderCircle } from "lucide-svelte";
import { onMount } from "svelte";
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

// Fixed pool size — matches the max completions returned (20 for search, ~10 for normal)
const POOL_SIZE = 20;
const POOL_INDICES = Array.from({ length: POOL_SIZE }, (_, i) => i);

let listEl: HTMLUListElement | undefined = $state();

// Track icon paths that failed to load — show fallback instead of broken image
let brokenIcons = $state(new Set<string>());

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
	const lastSlash = label.lastIndexOf("/");
	if (lastSlash === -1) return label;
	if (label.endsWith("/")) {
		const trimmed = label.slice(0, -1);
		const idx = trimmed.lastIndexOf("/");
		return idx === -1 ? `${trimmed}/` : `${trimmed.slice(idx + 1)}/`;
	}
	return label.slice(lastSlash + 1);
}

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
let hasItems = $derived(items.length > 0);
</script>

<ul class="completions" class:empty={!hasItems} role="listbox" bind:this={listEl}>
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
	<!-- Fixed pool: NO {#if} inside slots — all DOM nodes exist from first mount -->
	{#each POOL_INDICES as idx}
		{@const item = items[idx]}
		{@const active = idx < items.length}
		{@const label = item?.label ?? "\u00A0"}
		{@const isFolder = item?.icon_path === "__folder__"}
		{@const hasCustomIcon = !!(item?.icon_path && item.icon_path !== "__folder__")}
		{@const noIcon = !item?.icon_path}
		{@const iconKey = item?.icon_path ?? ""}
		{@const iconBroken = hasCustomIcon && brokenIcons.has(iconKey)}
		{@const showImg = hasCustomIcon && !iconBroken}
		{@const showFallback = noIcon || iconBroken}
		{@const search = searchDisplayName(label)}
		<li
			class="completion-item"
			class:selected={active && idx === selectedIndex}
			class:inactive={!active}
			onmousedown={(e) => e.preventDefault()}
			onclick={() => active && onselect(item.label)}
			onkeydown={(e) => e.key === "Enter" && active && onselect(item.label)}
			role="option"
			aria-selected={active && idx === selectedIndex}
			tabindex="-1"
		>
			<span class="icon">
				<span style:visibility={isFolder ? "visible" : "hidden"} class="icon-slot">
					<Folder size={20} strokeWidth={1.5} class="icon-folder" />
				</span>
				<span style:visibility={showImg ? "visible" : "hidden"} class="icon-slot">
					<img
						src={showImg && Math.abs(idx - selectedIndex) <= 3 ? (iconSrc(item.icon_path) ?? "") : "data:,"}
						alt="" width="24" height="24" decoding="async"
						onerror={() => { if (item?.icon_path) brokenIcons.add(item.icon_path); brokenIcons = brokenIcons; }}
					/>
				</span>
				<span style:visibility={showFallback ? "visible" : "hidden"} class="icon-slot">
					<AppWindow size={20} strokeWidth={1.5} class="icon-fallback" />
				</span>
			</span>
			<!-- Search-mode label group -->
			<div class="label-group" class:label-hidden={!isSearchMode}>
				<span class="label">{search.name}</span>
				<span class="search-path" style:visibility={search.parent ? "visible" : "hidden"}>{search.parent || "\u00A0"}</span>
			</div>
			<!-- Normal-mode label group -->
			<div class="label-group" class:label-hidden={isSearchMode}>
				<span class="label">{pathContext ? displayName(label) : label}</span>
				<span class="description" style:visibility={item?.description ? "visible" : "hidden"}>{item?.description ?? "\u00A0"}</span>
			</div>
		</li>
	{/each}
	<!-- Hints bar — kept with {#if} since it's not in the hot path -->
	<li class="hints" aria-hidden="true" class:inactive={items.length === 0}>
		<span class="hint"><kbd>↑↓</kbd> navigate</span>
		<span class="hint"><kbd>↵</kbd> {isSearchMode ? "open" : (items[selectedIndex]?.icon_path === "__folder__" ? "open folder" : "select")}</span>
		<span class="hint" style:visibility={(browseMode || isSearchMode) && items[selectedIndex]?.icon_path === "__folder__" ? "visible" : "hidden"}><kbd>tab</kbd> drill into</span>
		<span class="hint" style:visibility={isSearchMode || (browseMode && pathContext && pathContext !== "~/") ? "visible" : "hidden"}><kbd>⇧tab</kbd> go back</span>
		<span class="hint" style:visibility={scopeTabs.length > 1 ? "visible" : "hidden"}><kbd>ctrl+tab</kbd> switch scope</span>
		<span class="hint"><kbd>esc</kbd> dismiss</span>
	</li>
</ul>

<style>
	.completions {
		list-style: none;
		overflow-y: auto;
		flex: 1;
		min-height: 0;
		will-change: transform;
	}

	.completions.empty {
		visibility: hidden;
		max-height: 0;
		overflow: hidden;
		pointer-events: none;
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

	.inactive {
		max-height: 0;
		overflow: hidden;
		opacity: 0;
		pointer-events: none;
		padding: 0 !important;
		margin: 0 !important;
		border: none !important;
	}

	.label-hidden {
		visibility: hidden;
		position: absolute;
		pointer-events: none;
	}

	.completion-item {
		display: flex;
		align-items: center;
		gap: 12px;
		padding: 8px 20px 8px 18px;
		cursor: pointer;
		border-left: 2px solid transparent;
		position: relative;
	}

	.completion-item:hover {
		background: var(--bg-secondary);
		transition: background 80ms ease;
	}

	.completion-item.selected {
		background: var(--bg-secondary);
		border-left-color: var(--accent);
		padding-left: 20px;
	}

	.icon {
		width: 24px;
		height: 24px;
		position: relative;
		flex-shrink: 0;
	}

	.icon-slot {
		position: absolute;
		top: 0;
		left: 0;
		width: 24px;
		height: 24px;
		display: flex;
		align-items: center;
		justify-content: center;
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

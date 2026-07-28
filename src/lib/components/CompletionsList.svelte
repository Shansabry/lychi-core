<script lang="ts">
import { convertFileSrc } from "@tauri-apps/api/core";
import {
	AppWindow,
	Clock,
	Folder,
	Lightbulb,
	LoaderCircle,
	MessageSquare,
	Terminal,
	TriangleAlert,
	Zap,
} from "lucide-svelte";
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
	searchMode = false,
	metaMap = undefined,
	ignoreActive = false,
	flashMessage = "",
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
	searchMode?: boolean;
	metaMap?: Map<string, { size_bytes?: number | null; modified_secs?: number | null }>;
	ignoreActive?: boolean;
	flashMessage?: string;
} = $props();

// Fixed pool size — matches the max completions returned (20 for search, ~10 for normal)
const POOL_SIZE = 20;
const POOL_INDICES = Array.from({ length: POOL_SIZE }, (_, i) => i);

let listEl: HTMLUListElement | undefined = $state();

// Track icon load state — preloaded via Image() probe to avoid <img> placeholder border
let brokenIcons = $state(new Set<string>());
let loadedIcons = $state(new Set<string>());

function iconSrc(path: string | null): string | null {
	if (!path) return null;
	// Sentinel values are not file paths — never pass them to the asset protocol
	if (path.startsWith("__")) return null;
	if ("__TAURI_INTERNALS__" in window) {
		return convertFileSrc(path);
	}
	return path;
}

function preloadIcon(path: string | null | undefined) {
	if (!path) return;
	const src = iconSrc(path);
	if (!src || brokenIcons.has(path) || loadedIcons.has(path)) return;
	const probe = new Image();
	probe.onload = () => {
		loadedIcons = new Set(loadedIcons).add(path);
	};
	probe.onerror = () => {
		brokenIcons = new Set(brokenIcons).add(path);
	};
	probe.src = src;
}

// Count non-item rows above the list items for scroll offset
let headerRows = $derived((pathContext ? 1 : 0) + (scopeTabs.length > 1 ? 1 : 0));

// Preload icons whenever items change — must be in $effect so loadedIcons update triggers re-render
$effect(() => {
	for (const item of items) {
		preloadIcon(item.icon_path);
	}
});

// Scroll selected item into view when selection changes
$effect(() => {
	if (listEl && selectedIndex >= 0) {
		const item = listEl.children[selectedIndex + headerRows] as HTMLElement | undefined;
		item?.scrollIntoView({ block: "nearest" });
	}
});

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

let isSearchMode = $derived(searchMode || scopeTabs.length > 0);
let hasItems = $derived(items.length > 0);

function relativeTime(secs: number | null | undefined): string {
	if (!secs) return "";
	const diff = Math.floor(Date.now() / 1000) - secs;
	if (diff < 60) return "just now";
	if (diff < 3600) return `${Math.floor(diff / 60)}m ago`;
	if (diff < 86400) return `${Math.floor(diff / 3600)}h ago`;
	if (diff < 604800) return `${Math.floor(diff / 86400)}d ago`;
	return new Date(secs * 1000).toLocaleDateString();
}

function formatSize(bytes: number | null | undefined): string {
	if (!bytes) return "";
	if (bytes < 1024) return `${bytes} B`;
	if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(0)} KB`;
	return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}
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
			{#if ignoreActive}
				<span class="ignore-badge">.gitignore</span>
			{/if}
		</li>
	{:else if ignoreActive && isSearchMode}
		<li class="breadcrumb" aria-hidden="true">
			<span class="ignore-badge">.gitignore</span>
		</li>
	{/if}
	<!-- Fixed pool: NO {#if} inside slots — all DOM nodes exist from first mount -->
	{#each POOL_INDICES as idx}
		{@const item = items[idx]}
		{@const active = idx < items.length}
		{@const label = item?.label ?? "\u00A0"}
		{@const isSeparator = item?.icon_path === "__separator__"}
		{@const isFolder = item?.icon_path === "__folder__"}
		{@const isHistory = item?.icon_path === "__history__"}
		{@const isWarning = item?.icon_path === "__warning__"}
		{@const isContext = item?.icon_path === "__context__"}
		{@const isTerminal = item?.icon_path === "__terminal__"}
		{@const isInfo = item?.icon_path === "__info__"}
		{@const isClipImage = item?.icon_path === "__clipboard_image__"}
		{@const isAiChat = item?.icon_path === "__ai_chat__"}
		{@const isWeb = item?.label?.startsWith("Search web:")}
		{@const hideIcon = item?.icon_path === "__none__" || item?.icon_path === "__web__" || isWeb || isSeparator}
		{@const hasCustomIcon = !!(item?.icon_path && item.icon_path !== "__folder__" && item.icon_path !== "__none__" && item.icon_path !== "__web__" && item.icon_path !== "__history__" && item.icon_path !== "__separator__" && item.icon_path !== "__warning__" && item.icon_path !== "__context__" && item.icon_path !== "__terminal__" && item.icon_path !== "__info__" && item.icon_path !== "__clipboard_image__" && item.icon_path !== "__ai_chat__")}
		{@const noIcon = !item?.icon_path}
		{@const iconKey = item?.icon_path ?? ""}
		{@const iconBroken = hasCustomIcon && brokenIcons.has(iconKey)}
		{@const iconLoaded = hasCustomIcon && loadedIcons.has(iconKey)}
		{@const showImg = hasCustomIcon && iconLoaded && !iconBroken}
		{@const showFallback = (noIcon || iconBroken || (hasCustomIcon && !iconLoaded)) && !hideIcon}
		{@const search = searchDisplayName(label)}
		{@const meta = metaMap?.get(label)}
		{@const timeStr = relativeTime(meta?.modified_secs)}
		<li
			class="completion-item"
			class:completion-separator={active && isSeparator}
			class:completion-warning={active && isWarning}
			class:selected={active && idx === selectedIndex && !isSeparator && !isWarning && !isInfo}
			class:inactive={!active}
			onmousedown={(e) => e.preventDefault()}
			onclick={() => active && !isSeparator && !isWarning && !isInfo && onselect(item.label)}
			onkeydown={(e) => e.key === "Enter" && active && !isSeparator && !isWarning && !isInfo && onselect(item.label)}
			role={isSeparator ? "separator" : "option"}
			aria-selected={active && idx === selectedIndex && !isSeparator && !isWarning && !isInfo}
			tabindex="-1"
		>
			<!-- Separator layout -->
			<div class="separator-content" style:display={isSeparator ? "" : "none"}>
				<span class="separator-line"></span>
				<span class="separator-text">{label}</span>
				<span class="separator-line"></span>
			</div>
			<!-- Normal item layout -->
			<span class="icon" style:display={hideIcon ? "none" : ""}>
				<span style:visibility={isFolder ? "visible" : "hidden"} class="icon-slot">
					<Folder size={20} strokeWidth={1.5} class="icon-folder" />
				</span>
				<span style:visibility={isHistory ? "visible" : "hidden"} class="icon-slot">
					<Clock size={20} strokeWidth={1.5} class="icon-history" />
				</span>
				<span style:visibility={isWarning ? "visible" : "hidden"} class="icon-slot">
					<TriangleAlert size={20} strokeWidth={1.5} class="icon-warning" />
				</span>
				<span style:visibility={isContext ? "visible" : "hidden"} class="icon-slot">
					<Zap size={20} strokeWidth={1.5} class="icon-context" />
				</span>
				<span style:visibility={isTerminal ? "visible" : "hidden"} class="icon-slot">
					<Terminal size={20} strokeWidth={1.5} class="icon-terminal" />
				</span>
				<span style:visibility={isInfo ? "visible" : "hidden"} class="icon-slot">
					<Lightbulb size={20} strokeWidth={1.5} class="icon-info" />
				</span>
				<span style:visibility={isAiChat ? "visible" : "hidden"} class="icon-slot">
					<MessageSquare size={19} strokeWidth={1.5} class="icon-ai-chat" />
				</span>
				<span style:visibility={isClipImage && item?.thumb_b64 ? "visible" : "hidden"} class="icon-slot clip-thumb-slot">
					<img
						class="clip-thumb"
						src={isClipImage && item?.thumb_b64 ? `data:image/png;base64,${item.thumb_b64}` : ""}
						alt=""
					/>
				</span>
				<span
					class="icon-slot icon-img-slot"
					style:visibility={showImg ? "visible" : "hidden"}
					style:background-image={showImg ? `url('${iconSrc(item.icon_path)}')` : "none"}
					aria-hidden="true"
				></span>
				<span style:visibility={showFallback ? "visible" : "hidden"} class="icon-slot">
					<AppWindow size={20} strokeWidth={1.5} class="icon-fallback" />
				</span>
			</span>
			<!-- Search-mode label group -->
			<div class="label-group search-label-group" class:label-hidden={!isSearchMode || isSeparator}>
				<span class="label search-name">{search.name}</span>
				<span class="search-meta">
					<span class="type-badge" style:visibility={item?.description && !isFolder ? "visible" : "hidden"}>{item?.description ?? "\u00A0"}</span>
					<span class="meta-time" style:visibility={timeStr && !isFolder ? "visible" : "hidden"}>{timeStr || "\u00A0"}</span>
				</span>
				<span class="search-path" style:visibility={search.parent ? "visible" : "hidden"}>{search.parent || "\u00A0"}</span>
			</div>
			<!-- Normal-mode label group -->
			<div class="label-group" class:label-hidden={isSearchMode || isSeparator}>
				<span class="label" class:label-history={isHistory}>{pathContext ? displayName(label) : label}</span>
				<span
					class="description"
					class:confidence-badge={!isContext && !isWeb && !!item?.description}
					class:web-hint={isWeb && !!item?.description}
					style:visibility={item?.description ? "visible" : "hidden"}
				>{item?.description ?? "\u00A0"}</span>
				<span
					class="description reason-highlight"
					style:visibility={isContext && item?.reason ? "visible" : "hidden"}
					style:display={isContext && item?.reason ? "" : "none"}
				>{item?.reason ?? "\u00A0"}</span>
			</div>
		</li>
	{/each}
	<!-- Hints bar — kept with {#if} since it's not in the hot path -->
	<li class="hints" aria-hidden="true" class:inactive={items.length === 0}>
		{#if flashMessage}
			<span class="hint flash">{flashMessage}</span>
		{:else}
			<span class="hint"><kbd>↑↓</kbd> navigate</span>
			<span class="hint"><kbd>↵</kbd> {isSearchMode ? (items[selectedIndex]?.icon_path === "__folder__" ? "open folder" : "open") : (items[selectedIndex]?.icon_path === "__folder__" ? "open folder" : "select")}</span>
			<span class="hint" style:visibility={(isSearchMode || browseMode) && items[selectedIndex]?.icon_path === "__folder__" ? "visible" : "hidden"}><kbd>tab</kbd>/<kbd>→</kbd> drill into</span>
			<span class="hint" style:visibility={isSearchMode ? "visible" : "hidden"}><kbd>ctrl+↵</kbd> reveal</span>
			<span class="hint" style:visibility={isSearchMode ? "visible" : "hidden"}><kbd>ctrl+⇧+c</kbd> copy path</span>
			<span class="hint" style:visibility={isSearchMode || (browseMode && pathContext && pathContext !== "~/") ? "visible" : "hidden"}><kbd>⇧tab</kbd> go back</span>
			<span class="hint" style:visibility={scopeTabs.length > 1 ? "visible" : "hidden"}><kbd>ctrl+tab</kbd> switch scope</span>
			<span class="hint"><kbd>esc</kbd> dismiss</span>
		{/if}
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
		display: flex;
		align-items: center;
		gap: 8px;
	}

	.ignore-badge {
		font-family: var(--font-mono);
		font-size: 9px;
		color: var(--fg-muted);
		background: var(--bg-secondary);
		padding: 1px 6px;
		border-radius: 4px;
		opacity: 0.6;
		margin-left: auto;
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
		display: none !important;
	}

	.completion-item {
		display: flex;
		align-items: center;
		gap: 12px;
		padding: 8px 20px;
		height: 36px;
		box-sizing: border-box;
		cursor: pointer;
		position: relative;
		overflow: hidden;
	}

	.completion-separator {
		height: 24px;
		padding: 0 20px;
		cursor: default;
		pointer-events: none;
	}

	.completion-separator:hover {
		background: none;
	}

	.completion-warning {
		cursor: default;
		pointer-events: none;
		opacity: 0.85;
	}

	.separator-content {
		display: flex;
		align-items: center;
		gap: 8px;
		width: 100%;
	}

	.separator-line {
		flex: 1;
		height: 1px;
		background: var(--border);
		opacity: 0.4;
	}

	.separator-text {
		font-family: var(--font-mono);
		font-size: 10px;
		color: var(--fg-muted);
		opacity: 0.5;
		text-transform: lowercase;
		flex-shrink: 0;
	}

	.completion-item:hover {
		background: var(--bg-secondary);
		transition: background 80ms ease;
	}

	.completion-item.selected {
		background: var(--bg-secondary);
	}

	.completion-item.selected::before {
		content: '';
		position: absolute;
		left: 0;
		top: 0;
		bottom: 0;
		width: 2px;
		background: var(--accent);
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
	.clip-thumb {
		width: 24px;
		height: 24px;
		object-fit: contain;
		border-radius: 2px;
	}

	.icon-img-slot {
		background-size: contain;
		background-repeat: no-repeat;
		background-position: center;
	}

	.icon :global(.icon-fallback) {
		color: var(--fg-muted);
		opacity: 0.5;
	}

	.icon :global(.icon-folder) {
		color: var(--accent);
	}

	.icon :global(.icon-history) {
		color: var(--fg-muted);
		opacity: 0.6;
	}

	.icon :global(.icon-warning) {
		color: var(--warning);
		opacity: 0.9;
	}

	.icon :global(.icon-context) {
		color: var(--accent);
		opacity: 0.8;
	}
	.icon :global(.icon-ai-chat) {
		color: var(--accent);
	}

	.icon :global(.icon-terminal) {
		color: var(--fg-muted);
		opacity: 0.7;
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

	.search-label-group {
		gap: 6px;
	}

	.label {
		font-family: var(--font-mono);
		font-size: 14px;
		color: var(--fg);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	/* In search mode, filename should shrink to make room for meta + path */
	.search-name {
		flex-shrink: 1;
		min-width: 60px;
	}

	.description {
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--fg-muted);
		opacity: 0.7;
		flex-shrink: 0;
	}

	.description.reason-highlight {
		font-size: 10px;
		border: 1px solid var(--border);
		padding: 1px 8px;
		border-radius: 8px;
		opacity: 0.6;
	}

	/* App match confidence badge — "Open app?" / "Open app" */
	.description.confidence-badge {
		font-size: 10px;
		border: 1px solid var(--border);
		padding: 1px 8px;
		border-radius: 8px;
		opacity: 0.5;
		margin-left: auto;
	}

	/* Web search hint — "Enter to search" */
	.description.web-hint {
		font-size: 10px;
		color: var(--accent-blue);
		opacity: 0.65;
		margin-left: auto;
	}

	/* History item labels dimmed vs live results */
	.label.label-history {
		opacity: 0.65;
	}

	.search-meta {
		display: flex;
		align-items: baseline;
		gap: 6px;
		flex-shrink: 0;
	}

	.type-badge {
		font-family: var(--font-mono);
		font-size: 10px;
		color: var(--accent);
		background: var(--bg-secondary);
		padding: 1px 6px;
		border-radius: 4px;
		flex-shrink: 0;
		opacity: 0.7;
	}

	.meta-time {
		font-family: var(--font-mono);
		font-size: 10px;
		color: var(--fg-muted);
		opacity: 0.6;
		flex-shrink: 0;
		white-space: nowrap;
	}

	.search-path {
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--fg-muted);
		opacity: 0.6;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
		margin-left: auto;
		flex-shrink: 1;
		min-width: 0;
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
		opacity: 0.6;
	}

	.hint kbd {
		font-family: inherit;
		font-size: inherit;
		opacity: 0.7;
	}

	.hint.flash {
		color: var(--accent);
		opacity: 1;
	}
</style>

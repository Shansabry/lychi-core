<script lang="ts">
import { convertFileSrc } from "@tauri-apps/api/core";
import { File, Folder } from "lucide-svelte";
import type { CompletionItem } from "$lib/ipc";

let {
	items,
	selectedIndex,
	onselect,
}: {
	items: CompletionItem[];
	selectedIndex: number;
	onselect: (label: string) => void;
} = $props();

function iconSrc(path: string | null): string | null {
	if (!path) return null;
	if ("__TAURI_INTERNALS__" in window) {
		return convertFileSrc(path);
	}
	return path;
}
</script>

<ul class="completions" role="listbox">
	{#each items as item, i}
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
					<Folder size={18} strokeWidth={1.5} class="icon-folder" />
				{:else if item.icon_path}
					<img src={iconSrc(item.icon_path)} alt="" />
				{:else}
					<span class="icon-fallback">⬡</span>
				{/if}
			</span>
			<span class="label">{item.label}</span>
		</li>
	{/each}
</ul>

<style>
	.completions {
		list-style: none;
		overflow-y: auto;
		flex: 1;
		min-height: 0;
	}

	.completion-item {
		display: flex;
		align-items: center;
		gap: 12px;
		padding: 8px 20px;
		cursor: pointer;
		transition: background 60ms ease;
	}

	.completion-item:hover {
		background: var(--bg-secondary);
	}

	.completion-item.selected {
		background: var(--bg-secondary);
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

	.icon-fallback {
		color: var(--fg-muted);
		font-size: 16px;
	}

	.icon :global(.icon-folder) {
		color: var(--accent);
	}

	.label {
		font-family: var(--font-mono);
		font-size: 14px;
		color: var(--fg);
	}

	.completion-item.selected .label {
		color: var(--accent);
	}
</style>

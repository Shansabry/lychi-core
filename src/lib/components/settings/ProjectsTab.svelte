<script lang="ts">
import { Check, ChevronRight, FolderOpen, X } from "lucide-svelte";
import type { DirEntry } from "$lib/ipc";
import { listDirectories, saveProjectsConfig } from "$lib/ipc";

let {
	projectDirs = $bindable(),
	onsaveerror,
}: {
	projectDirs: string[];
	onsaveerror: (msg: string) => void;
} = $props();

let browsing = $state(false);
let browsePath = $state("");
let browseDirs: DirEntry[] = $state([]);
let browseInput = $state("");

async function saveProjectDirs() {
	try {
		await saveProjectsConfig({ directories: projectDirs });
	} catch (err) {
		console.error("[settings] Failed to save projects config:", err);
		onsaveerror(`Failed to save: ${err}`);
	}
}

async function openBrowser() {
	browsing = true;
	browseInput = "";
	browsePath = "";
	browseDirs = await listDirectories("");
}

async function navigateTo(path: string) {
	browsePath = path;
	browseInput = path;
	browseDirs = await listDirectories(path);
}

async function goUp() {
	if (!browsePath || browsePath === "/") return;
	const parent = browsePath.replace(/\/[^/]+\/?$/, "") || "/";
	await navigateTo(parent);
}

async function handleBrowseInput() {
	const val = browseInput.trim();
	if (!val) {
		await navigateTo("");
		return;
	}
	browsePath = val;
	browseDirs = await listDirectories(val);
}

function selectCurrentDir() {
	const selected = browsePath || "/";
	if (!projectDirs.includes(selected)) {
		projectDirs = [...projectDirs, selected];
		saveProjectDirs();
	}
	browsing = false;
}

function removeProjectDir(index: number) {
	projectDirs = projectDirs.filter((_, i) => i !== index);
	saveProjectDirs();
}
</script>

{#if browsing}
	<div class="browser-header">
		<button class="browser-up" onclick={goUp} disabled={!browsePath || browsePath === "/"} title="Go up">
			..
		</button>
		<input
			class="browser-input"
			type="text"
			bind:value={browseInput}
			placeholder="/ or ~/path..."
			spellcheck="false"
			onkeydown={(e) => { if (e.key === "Enter") { e.preventDefault(); handleBrowseInput(); } }}
		/>
		<button class="browser-select" onclick={selectCurrentDir} title="Select this folder">
			<Check size={12} strokeWidth={2} />
		</button>
		<button class="browser-close" onclick={() => (browsing = false)} title="Cancel">
			<X size={12} strokeWidth={2} />
		</button>
	</div>
	<div class="browser-list">
		{#each browseDirs as dir}
			<button class="browser-item" onclick={() => navigateTo(dir.path)}>
				<FolderOpen size={12} strokeWidth={1.5} />
				<span>{dir.name}</span>
				<ChevronRight size={12} strokeWidth={1.5} />
			</button>
		{:else}
			<div class="browser-empty">No subdirectories</div>
		{/each}
	</div>
{:else}
	<div class="dir-list">
		{#each projectDirs as dir, i}
			<div class="dir-item">
				<span class="dir-path">{dir}</span>
				<button class="dir-remove" onclick={() => removeProjectDir(i)} title="Remove">
					<X size={12} strokeWidth={2} />
				</button>
			</div>
		{/each}
	</div>
	<button class="browse-btn" onclick={openBrowser}>
		<FolderOpen size={13} strokeWidth={1.5} />
		Browse folder
	</button>
{/if}

<style>
	.dir-list {
		display: flex;
		flex-direction: column;
		gap: 2px;
		margin-bottom: 10px;
	}

	.dir-item {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 8px;
		padding: 5px 8px;
		background: var(--bg-secondary);
		border-radius: 4px;
	}

	.dir-path {
		font-size: 12px;
		color: var(--fg);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		min-width: 0;
	}

	.dir-remove {
		display: flex;
		align-items: center;
		justify-content: center;
		background: none;
		border: none;
		color: var(--fg-muted);
		cursor: pointer;
		padding: 2px;
		border-radius: 3px;
		flex-shrink: 0;
		transition: color 100ms ease;
	}

	.dir-remove:hover {
		color: var(--error);
	}

	.browse-btn {
		display: flex;
		align-items: center;
		gap: 6px;
		background: var(--bg);
		color: var(--fg-muted);
		border: 1px dashed var(--border);
		border-radius: 4px;
		padding: 6px 12px;
		font-family: var(--font-mono);
		font-size: 12px;
		cursor: pointer;
		width: 100%;
		justify-content: center;
	}

	.browse-btn:hover {
		color: var(--fg);
		border-color: var(--fg-muted);
	}

	.browser-header {
		display: flex;
		align-items: center;
		gap: 6px;
		margin-bottom: 6px;
	}

	.browser-input {
		flex: 1;
		min-width: 0;
		font-size: 11px;
		font-family: var(--font-mono);
		color: var(--fg);
		background: var(--bg-secondary);
		border: 1px solid var(--border);
		border-radius: 4px;
		padding: 3px 6px;
		outline: none;
	}

	.browser-input:focus {
		border-color: var(--fg-muted);
	}

	.browser-up {
		background: var(--bg-secondary);
		color: var(--fg-muted);
		border: 1px solid var(--border);
		border-radius: 4px;
		padding: 3px 8px;
		font-family: var(--font-mono);
		font-size: 11px;
		cursor: pointer;
		flex-shrink: 0;
	}

	.browser-up:hover:not(:disabled) {
		color: var(--fg);
		border-color: var(--fg-muted);
	}

	.browser-up:disabled {
		opacity: 0.3;
		cursor: default;
	}

	.browser-select,
	.browser-close {
		display: flex;
		align-items: center;
		justify-content: center;
		background: none;
		border: 1px solid var(--border);
		border-radius: 4px;
		padding: 3px 6px;
		cursor: pointer;
		flex-shrink: 0;
	}

	.browser-select {
		color: var(--accent);
	}

	.browser-select:hover {
		border-color: var(--accent);
	}

	.browser-close {
		color: var(--fg-muted);
	}

	.browser-close:hover {
		color: var(--error);
		border-color: var(--error);
	}

	.browser-list {
		display: flex;
		flex-direction: column;
		gap: 1px;
		max-height: 200px;
		overflow-y: auto;
	}

	.browser-item {
		display: flex;
		align-items: center;
		gap: 6px;
		padding: 4px 8px;
		background: none;
		border: none;
		color: var(--fg);
		font-family: var(--font-mono);
		font-size: 12px;
		cursor: pointer;
		border-radius: 3px;
		text-align: left;
	}

	.browser-item:hover {
		background: var(--bg-secondary);
	}

	.browser-item span {
		flex: 1;
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.browser-empty {
		font-size: 11px;
		color: var(--fg-muted);
		padding: 8px;
		text-align: center;
	}
</style>

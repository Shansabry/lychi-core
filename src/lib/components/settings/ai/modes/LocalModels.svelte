<script lang="ts">
import { listen } from "@tauri-apps/api/event";
import { onMount } from "svelte";
import type { AiConfig, LocalModelInfo } from "$lib/ipc";
import { deleteLocalModel, downloadLocalModel, getLocalModels } from "$lib/ipc";
import AdvancedFields from "../AdvancedFields.svelte";
import ExperimentalNote from "../ExperimentalNote.svelte";

let {
	aiConfig = $bindable(),
	advancedOpen = $bindable(false),
	onsave,
	onsaveerror,
}: {
	aiConfig: AiConfig;
	advancedOpen?: boolean;
	onsave: () => void;
	onsaveerror: (msg: string) => void;
} = $props();

let localModels: LocalModelInfo[] = $state([]);
let pendingDownload: LocalModelInfo | null = $state(null);
let downloadProgress: Record<
	string,
	{ downloaded: number; total: number | null; error: string | null }
> = $state({});
let selectedIndex = $state(0);

export async function refreshLocalModels() {
	try {
		localModels = await getLocalModels();
	} catch {
		localModels = [];
	}
}

function fmtBytes(n: number): string {
	if (n >= 1e9) return `${(n / 1e9).toFixed(1)} GB`;
	if (n >= 1e6) return `${(n / 1e6).toFixed(0)} MB`;
	return `${(n / 1e3).toFixed(0)} KB`;
}

function askDownload(m: LocalModelInfo) {
	pendingDownload = m;
}

async function confirmDownload() {
	const m = pendingDownload;
	pendingDownload = null;
	if (!m) return;
	downloadProgress[m.id] = { downloaded: 0, total: null, error: null };
	try {
		await downloadLocalModel(m.id); // returns immediately; progress via event
	} catch (err) {
		downloadProgress[m.id] = { downloaded: 0, total: null, error: `${err}` };
	}
}

async function handleSelectLocalModel(m: LocalModelInfo) {
	if (!m.downloaded) return;
	aiConfig.local_model = `${m.id}.gguf`;
	onsave();
}

async function handleDeleteLocalModel(m: LocalModelInfo) {
	try {
		await deleteLocalModel(m.id);
		if (aiConfig.local_model === `${m.id}.gguf`) {
			aiConfig.local_model = "";
			onsave();
		}
		await refreshLocalModels();
	} catch (err) {
		onsaveerror(`Failed to delete: ${err}`);
	}
}

/** ↵ on the selected row: use if downloaded, else open the download gate. */
export function activateSelected() {
	const m = localModels[selectedIndex];
	if (!m) return;
	if (m.downloaded) handleSelectLocalModel(m);
	else if (!downloadProgress[m.id]) askDownload(m);
}

/** ⌫ on the selected row: remove a downloaded model (guarded by warning gate). */
export function removeSelected() {
	const m = localModels[selectedIndex];
	if (m?.downloaded) handleDeleteLocalModel(m);
}

export function moveSelection(delta: number) {
	if (localModels.length === 0) return;
	selectedIndex = Math.max(0, Math.min(localModels.length - 1, selectedIndex + delta));
}

onMount(() => {
	refreshLocalModels();
	const unlisten = listen<{
		model_id: string;
		file_label: string;
		downloaded: number;
		total: number | null;
		done: boolean;
		error: string | null;
	}>("lychi://model-download-progress", (e) => {
		const p = e.payload;
		if (p.error) {
			downloadProgress[p.model_id] = { downloaded: 0, total: null, error: p.error };
			return;
		}
		// "weights" is the last (big) file; its `done` means the model is ready.
		if (p.done && p.file_label === "weights") {
			delete downloadProgress[p.model_id];
			downloadProgress = { ...downloadProgress };
			refreshLocalModels();
			return;
		}
		if (!p.done) {
			downloadProgress[p.model_id] = {
				downloaded: p.downloaded,
				total: p.total,
				error: null,
			};
		}
	});
	return () => {
		unlisten.then((u) => u());
	};
});
</script>

<ExperimentalNote>
	Local models run entirely on your machine, so they're smaller and less capable
	than cloud models — expect simpler reasoning and the occasional miss on complex
	commands. Best for private, offline use; connect a cloud or API-key model for
	the full Lychi experience.
</ExperimentalNote>

<div class="section-label">Models</div>
<div class="cards">
	{#each localModels as m, i (m.id)}
		{@const prog = downloadProgress[m.id]}
		{@const active = aiConfig.local_model === `${m.id}.gguf`}
		<div class="card" class:selected={i === selectedIndex} class:active>
			<div class="ic">{active ? "◆" : "◇"}</div>
			<div class="main">
				<div class="name">
					{m.label}
					{#if active}<span class="tag active">active</span>{/if}
				</div>
				{#if prog?.error}
					<div class="meta err">✗ {prog.error}</div>
				{:else if prog}
					<div class="meta">
						downloading… {fmtBytes(prog.downloaded)}{prog.total
							? ` / ${fmtBytes(prog.total)}`
							: ""}
					</div>
					<div class="progress">
						<i
							style={prog.total
								? `width:${Math.round((prog.downloaded / prog.total) * 100)}%`
								: "width:20%"}
						></i>
					</div>
				{:else if m.downloaded}
					<div class="meta">{m.size_label} · downloaded · {m.ram_label}</div>
				{:else}
					<div class="meta">{m.size_label} · not downloaded</div>
				{/if}
			</div>
			{#if prog?.error}
				<button class="btn" onclick={() => askDownload(m)}>Retry</button>
			{:else if prog}
				<!-- in-flight: no action button -->
			{:else if m.downloaded}
				{#if active}
					<button class="btn ghost" onclick={() => handleDeleteLocalModel(m)}>Remove</button>
				{:else}
					<button class="btn" onclick={() => handleSelectLocalModel(m)}>Use</button>
				{/if}
			{:else}
				<button class="btn" onclick={() => askDownload(m)}>Download</button>
			{/if}
		</div>
	{/each}
</div>

{#if pendingDownload}
	<div class="warn-gate">
		<div class="warn-title">Download {pendingDownload.label}?</div>
		<ul class="warn-list">
			<li>Downloads <strong>{pendingDownload.size_label}</strong> (one-time).</li>
			<li>
				Runs on your <strong>CPU</strong> — uses ~{pendingDownload.ram_label} while active,
				best on a modern machine with 8&nbsp;GB+ RAM. May be slow on older hardware and uses
				battery.
			</li>
			<li><strong>Fully private &amp; offline</strong> — nothing is sent anywhere.</li>
		</ul>
		<div class="warn-actions">
			<button class="btn ai" onclick={confirmDownload}>Download</button>
			<button class="btn ghost" onclick={() => (pendingDownload = null)}>Cancel</button>
		</div>
	</div>
{/if}

<AdvancedFields bind:open={advancedOpen} bind:aiConfig {onsave} />

<style>
	.section-label {
		font-size: 10.5px;
		letter-spacing: 1.2px;
		text-transform: uppercase;
		color: var(--fg-muted);
		margin: 20px 0 12px;
	}
	.cards {
		display: flex;
		flex-direction: column;
		gap: 8px;
	}
	.card {
		display: flex;
		align-items: center;
		gap: 13px;
		padding: 13px 15px;
		background: var(--bg-secondary);
		border: 1px solid var(--border);
		border-radius: 9px;
	}
	.card.selected {
		border-color: color-mix(in srgb, var(--fg-muted) 60%, transparent);
	}
	.card.active {
		border-color: color-mix(in srgb, var(--accent) 45%, transparent);
		background: color-mix(in srgb, var(--accent) 7%, var(--bg-secondary));
	}
	.ic {
		width: 34px;
		height: 34px;
		border-radius: 8px;
		flex-shrink: 0;
		display: grid;
		place-items: center;
		background: color-mix(in srgb, var(--fg) 5%, var(--bg));
		color: var(--fg-muted);
		font-size: 15px;
	}
	.card.active .ic {
		color: var(--accent);
	}
	.main {
		flex: 1;
		min-width: 0;
	}
	.name {
		font-size: 12.5px;
		font-weight: 600;
		color: var(--fg);
		display: flex;
		align-items: center;
		gap: 8px;
	}
	.meta {
		color: var(--fg-muted);
		font-size: 11px;
		margin-top: 2px;
		font-family: var(--font-mono);
	}
	.meta.err {
		color: var(--error);
	}
	.tag {
		font-size: 9.5px;
		font-family: var(--font-mono);
		padding: 1px 6px;
		border-radius: 4px;
		background: color-mix(in srgb, var(--fg) 5%, var(--bg));
		color: var(--fg-muted);
		border: 1px solid var(--border);
	}
	.tag.active {
		color: var(--accent);
		border-color: color-mix(in srgb, var(--accent) 35%, transparent);
	}
	.progress {
		height: 4px;
		background: color-mix(in srgb, var(--fg) 8%, var(--bg));
		border-radius: 3px;
		overflow: hidden;
		margin-top: 8px;
	}
	.progress i {
		display: block;
		height: 100%;
		background: var(--accent);
		border-radius: 3px;
	}
	.btn {
		font-family: var(--font-mono);
		font-size: 12px;
		color: var(--fg);
		background: color-mix(in srgb, var(--fg) 5%, var(--bg));
		border: 1px solid var(--border);
		border-radius: 7px;
		padding: 6px 12px;
		cursor: pointer;
		flex-shrink: 0;
	}
	.btn:hover {
		border-color: var(--fg-muted);
	}
	.btn.ai {
		color: var(--accent);
		border-color: color-mix(in srgb, var(--accent) 35%, transparent);
	}
	.btn.ghost {
		background: transparent;
	}
	.warn-gate {
		border: 1px solid color-mix(in srgb, var(--accent) 40%, transparent);
		border-radius: 9px;
		padding: 14px;
		margin-top: 12px;
		background: var(--bg-secondary);
	}
	.warn-title {
		font-size: 13px;
		color: var(--fg);
		margin-bottom: 8px;
	}
	.warn-list {
		margin: 0 0 10px;
		padding-left: 18px;
		font-size: 12px;
		color: var(--fg-muted);
		line-height: 1.6;
	}
	.warn-list strong {
		color: var(--fg);
	}
	.warn-actions {
		display: flex;
		gap: 8px;
	}
</style>

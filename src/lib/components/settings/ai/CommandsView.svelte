<script lang="ts">
import { onMount } from "svelte";
import type { AiPresetItem } from "$lib/ipc";
import { addAiPreset, deleteAiPreset, getAiPresets, updateAiPreset } from "$lib/ipc";

let {
	onpresetchange,
	oncount,
}: {
	/** Called after any change so the launcher can reload its preset list. */
	onpresetchange?: () => void;
	/** Reports the current preset count up to the shell (for the nav badge). */
	oncount?: (n: number) => void;
} = $props();

let presets: AiPresetItem[] = $state([]);
// null = editing the "new command" draft; otherwise the id being edited.
let editingId: string | null = $state(null);
let keyword = $state("");
let name = $state("");
let template = $state("");
let error: string | null = $state(null);
let confirmingDelete = $state(false);

const isNew = $derived(editingId === null);

onMount(() => {
	getAiPresets().then((ps) => {
		presets = ps.sort((a, b) => a.keyword.localeCompare(b.keyword));
		oncount?.(presets.length);
		if (presets.length > 0) startEdit(presets[0]);
	});
});

function report() {
	oncount?.(presets.length);
	onpresetchange?.();
}

function startNew() {
	editingId = null;
	keyword = "";
	name = "";
	template = "";
	error = null;
	confirmingDelete = false;
}

function startEdit(p: AiPresetItem) {
	editingId = p.id;
	keyword = p.keyword;
	name = p.name;
	template = p.template;
	error = null;
	confirmingDelete = false;
}

export async function save() {
	const k = keyword.trim();
	const n = name.trim();
	const t = template.trim();
	if (!k || !n || !t) {
		error = "Keyword, name, and template are all required.";
		return;
	}
	try {
		if (editingId === null) {
			const item = await addAiPreset(k, n, t);
			presets = [...presets, item].sort((a, b) => a.keyword.localeCompare(b.keyword));
			startEdit(item);
		} else {
			const id = editingId;
			await updateAiPreset(id, k, n, t);
			presets = presets
				.map((p) => (p.id === id ? { ...p, keyword: k, name: n, template: t } : p))
				.sort((a, b) => a.keyword.localeCompare(b.keyword));
		}
		error = null;
		report();
	} catch (err) {
		error = err instanceof Error ? err.message : String(err);
	}
}

export async function deleteSelected() {
	if (editingId === null) return;
	if (!confirmingDelete) {
		confirmingDelete = true;
		return;
	}
	const id = editingId;
	try {
		await deleteAiPreset(id);
		presets = presets.filter((p) => p.id !== id);
		report();
		if (presets.length > 0) startEdit(presets[0]);
		else startNew();
	} catch (err) {
		error = err instanceof Error ? err.message : String(err);
	}
}

/** ↑↓ list navigation. `null` selects the "+ New command" row. */
export function moveSelection(delta: number) {
	const idx = editingId === null ? presets.length : presets.findIndex((p) => p.id === editingId);
	const next = Math.max(0, Math.min(presets.length, idx + delta));
	if (next === presets.length) startNew();
	else startEdit(presets[next]);
}

export function dismissConfirm(): boolean {
	if (confirmingDelete) {
		confirmingDelete = false;
		return true;
	}
	return false;
}
</script>

<div class="cmd-layout">
	<div class="cmd-list">
		{#each presets as p, i (p.id)}
			<button
				class="cmd-item"
				class:active={editingId === p.id}
				onclick={() => startEdit(p)}
			>
				<span class="cmd-kw">{p.keyword}</span>
				<span class="cmd-shortcut kbd">{i + 1}</span>
			</button>
		{/each}
		<button class="cmd-item cmd-new" class:active={isNew} onclick={startNew}>
			+ New command
			<span class="cmd-shortcut kbd">↵</span>
		</button>
	</div>

	<div class="cmd-detail">
		<div class="field">
			<label for="cmd-kw">Keyword</label>
			<input
				id="cmd-kw"
				class="control"
				bind:value={keyword}
				placeholder="keyword"
				type="text"
				maxlength={24}
				spellcheck="false"
			/>
		</div>
		<div class="field">
			<label for="cmd-name">Name</label>
			<input
				id="cmd-name"
				class="control"
				bind:value={name}
				placeholder="Display name"
				type="text"
				maxlength={40}
			/>
		</div>
		<div class="field top">
			<label for="cmd-tmpl">Template</label>
			<textarea
				id="cmd-tmpl"
				class="control tmpl"
				bind:value={template}
				placeholder={"Prompt template… use {input} for the user's text"}
				maxlength={5000}
			></textarea>
		</div>
		{#if error}
			<div class="err">{error}</div>
		{/if}
		<div class="actions">
			{#if !isNew}
				<button
					class="btn del"
					class:confirming={confirmingDelete}
					onclick={deleteSelected}
				>
					{confirmingDelete ? "Sure?" : "Delete"}
					<span class="kbd">⌫</span>
				</button>
			{/if}
			<div class="spacer"></div>
			<button class="btn ai" onclick={save}>
				Save <span class="kbd">⌘↵</span>
			</button>
		</div>
	</div>
</div>

<style>
	.cmd-layout {
		display: grid;
		grid-template-columns: 200px 1fr;
		border: 1px solid var(--border);
		border-radius: 10px;
		overflow: hidden;
		min-height: 300px;
	}
	.cmd-list {
		background: var(--bg-secondary);
		border-right: 1px solid var(--border);
		padding: 8px;
		display: flex;
		flex-direction: column;
		gap: 2px;
	}
	.cmd-item {
		display: flex;
		align-items: center;
		gap: 9px;
		padding: 8px 10px;
		border-radius: 7px;
		cursor: pointer;
		background: none;
		border: none;
		font-family: var(--font-mono);
		text-align: left;
		width: 100%;
	}
	.cmd-item:hover {
		background: color-mix(in srgb, var(--fg) 5%, transparent);
	}
	.cmd-item.active {
		background: color-mix(in srgb, var(--fg) 8%, var(--bg));
	}
	.cmd-kw {
		font-family: var(--font-mono);
		font-size: 12px;
		color: var(--fg);
	}
	.cmd-item.active .cmd-kw {
		color: var(--accent);
	}
	.cmd-shortcut {
		margin-left: auto;
	}
	.cmd-new {
		color: var(--fg-muted);
		font-size: 12px;
		margin-top: 4px;
		border-top: 1px dashed var(--border);
		padding-top: 10px;
		border-radius: 0 0 7px 7px;
	}
	.kbd {
		font-family: var(--font-mono);
		font-size: 10px;
		color: var(--fg-muted);
		background: color-mix(in srgb, var(--fg) 5%, var(--bg));
		border: 1px solid var(--border);
		border-radius: 4px;
		padding: 0 4px;
	}
	.cmd-detail {
		padding: 16px 18px;
	}
	.field {
		display: grid;
		grid-template-columns: 90px 1fr;
		align-items: center;
		gap: 12px;
		padding: 7px 0;
	}
	.field.top {
		align-items: start;
	}
	.field.top label {
		padding-top: 8px;
	}
	label {
		font-size: 12.5px;
		color: var(--fg-muted);
	}
	.control {
		font-family: var(--font-mono);
		font-size: 12.5px;
		color: var(--fg);
		background: var(--bg-secondary);
		border: 1px solid var(--border);
		border-radius: 7px;
		padding: 8px 11px;
		width: 100%;
		outline: none;
	}
	.control:focus {
		border-color: var(--fg-muted);
	}
	.tmpl {
		min-height: 88px;
		resize: vertical;
		line-height: 1.6;
	}
	.err {
		font-size: 11px;
		color: var(--error);
		padding: 4px 0 0 102px;
	}
	.actions {
		display: flex;
		gap: 8px;
		margin-top: 16px;
		padding-top: 14px;
		border-top: 1px solid color-mix(in srgb, var(--border) 60%, transparent);
	}
	.spacer {
		flex: 1;
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
		display: inline-flex;
		align-items: center;
		gap: 7px;
	}
	.btn:hover {
		border-color: var(--fg-muted);
	}
	.btn.ai {
		color: var(--accent);
		border-color: color-mix(in srgb, var(--accent) 35%, transparent);
	}
	.btn.del {
		background: transparent;
		color: var(--error);
		border-color: color-mix(in srgb, var(--error) 30%, transparent);
	}
	.btn.del.confirming {
		border-color: var(--error);
	}
	.btn .kbd {
		border-color: var(--border);
	}
</style>

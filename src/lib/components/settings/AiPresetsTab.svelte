<script lang="ts">
import { Plus, Sparkles, X } from "lucide-svelte";
import { onMount } from "svelte";
import type { AiPresetItem } from "$lib/ipc";
import { addAiPreset, deleteAiPreset, getAiPresets, updateAiPreset } from "$lib/ipc";

let {
	/** Called after any change so the launcher can reload its preset list. */
	onchange,
}: {
	onchange?: () => void;
} = $props();

let presets: AiPresetItem[] = $state([]);
let keyword = $state("");
let name = $state("");
let template = $state("");
let editing: AiPresetItem | null = $state(null);
let error: string | null = $state(null);

onMount(() => {
	getAiPresets().then((ps) => {
		presets = ps;
	});
});

function reset() {
	editing = null;
	keyword = "";
	name = "";
	template = "";
	error = null;
}

async function handleAdd() {
	const k = keyword.trim();
	const n = name.trim();
	const t = template.trim();
	if (!k || !n || !t) return;
	try {
		const item = await addAiPreset(k, n, t);
		presets = [...presets, item].sort((a, b) => a.keyword.localeCompare(b.keyword));
		reset();
		onchange?.();
	} catch (err) {
		error = err instanceof Error ? err.message : String(err);
	}
}

function startEdit(p: AiPresetItem) {
	editing = p;
	keyword = p.keyword;
	name = p.name;
	template = p.template;
	error = null;
}

async function handleSaveEdit() {
	if (!editing) return;
	const k = keyword.trim();
	const n = name.trim();
	const t = template.trim();
	if (!k || !n || !t) return;
	try {
		await updateAiPreset(editing.id, k, n, t);
		presets = presets
			.map((p) => (p.id === editing?.id ? { ...p, keyword: k, name: n, template: t } : p))
			.sort((a, b) => a.keyword.localeCompare(b.keyword));
		reset();
		onchange?.();
	} catch (err) {
		error = err instanceof Error ? err.message : String(err);
	}
}

async function handleDelete(id: string) {
	try {
		await deleteAiPreset(id);
		presets = presets.filter((p) => p.id !== id);
		if (editing?.id === id) reset();
		onchange?.();
	} catch (err) {
		error = err instanceof Error ? err.message : String(err);
	}
}
</script>

<div class="section">
	<p class="intro">
		AI Commands — type a <strong>keyword</strong> then your text to run a saved prompt.
		Use <code>{"{input}"}</code> in the template where your text should go.
		Example: keyword <code>email</code>, template
		<code>Write a professional email about: {"{input}"}</code>.
	</p>

	<div class="preset-form">
		{#if editing}
			<div class="form-header">
				<span class="form-title">Edit preset</span>
				<button class="cancel" onclick={reset} onmousedown={(e) => e.preventDefault()} tabindex={-1}>Cancel</button>
			</div>
		{/if}
		<div class="row">
			<input
				class="kw-input"
				bind:value={keyword}
				placeholder="keyword"
				type="text"
				maxlength={24}
				onkeydown={(e) => { if (e.key === "Enter") { e.preventDefault(); e.stopPropagation(); } }}
			/>
			<input
				class="name-input"
				bind:value={name}
				placeholder="Display name"
				type="text"
				maxlength={40}
				onkeydown={(e) => { if (e.key === "Enter") { e.preventDefault(); e.stopPropagation(); } }}
			/>
		</div>
		<textarea
			class="tpl-input"
			bind:value={template}
			placeholder={"Prompt template… use {input} for the user's text (Ctrl+Enter to save)"}
			maxlength={5000}
			rows={3}
			onkeydown={(e) => { if (e.key === "Enter" && e.ctrlKey) { e.preventDefault(); e.stopPropagation(); editing ? handleSaveEdit() : handleAdd(); } }}
		></textarea>
		{#if error}
			<div class="form-error">{error}</div>
		{/if}
		<button
			class="save-btn"
			onclick={() => (editing ? handleSaveEdit() : handleAdd())}
			onmousedown={(e) => e.preventDefault()}
			tabindex={-1}
		>
			{#if editing}
				Save changes
			{:else}
				<Plus size={12} strokeWidth={1.5} /> Add preset
			{/if}
		</button>
	</div>

	{#if presets.length > 0}
		<ul class="preset-list" role="list">
			{#each presets as p (p.id)}
				<li class="preset-item">
					<div class="preset-content">
						<span class="preset-kw"><Sparkles size={11} strokeWidth={2} /> {p.keyword}</span>
						<span class="preset-name">{p.name}</span>
						<span class="preset-preview">{p.template.split("\n")[0]?.slice(0, 60) || ""}</span>
					</div>
					<button
						class="preset-edit"
						onclick={() => startEdit(p)}
						onmousedown={(e) => e.preventDefault()}
						tabindex={-1}
						aria-label="Edit"
					>Edit</button>
					<button
						class="preset-delete"
						onclick={() => handleDelete(p.id)}
						onmousedown={(e) => e.preventDefault()}
						tabindex={-1}
						aria-label="Delete"
					>
						<X size={12} strokeWidth={1.5} />
					</button>
				</li>
			{/each}
		</ul>
	{:else}
		<div class="preset-empty">No AI presets yet. Add one above.</div>
	{/if}
</div>

<style>
	.section {
		padding: 10px 20px;
	}
	.intro {
		font-family: var(--font-sans, system-ui);
		font-size: 12px;
		color: var(--fg-muted);
		line-height: 1.5;
		margin: 0 0 12px;
	}
	.intro code {
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--fg);
		background: var(--bg-secondary);
		padding: 1px 4px;
		border-radius: 3px;
	}
	.preset-form {
		display: flex;
		flex-direction: column;
		gap: 6px;
		margin-bottom: 12px;
	}
	.form-header {
		display: flex;
		justify-content: space-between;
		align-items: center;
	}
	.form-title {
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--fg-muted);
		text-transform: uppercase;
		letter-spacing: 0.3px;
	}
	.cancel {
		font-family: var(--font-mono);
		font-size: 10px;
		color: var(--fg-muted);
		background: none;
		border: none;
		cursor: pointer;
		padding: 2px 6px;
	}
	.cancel:hover {
		color: var(--fg);
	}
	.row {
		display: flex;
		gap: 6px;
	}
	.kw-input {
		flex: 0 0 34%;
	}
	.name-input {
		flex: 1;
	}
	.kw-input,
	.name-input,
	.tpl-input {
		background: var(--bg-secondary);
		border: 1px solid var(--border);
		border-radius: 6px;
		color: var(--fg);
		font-family: var(--font-mono);
		font-size: 12.5px;
		padding: 6px 10px;
		outline: none;
	}
	.tpl-input {
		width: 100%;
		resize: none;
		line-height: 1.5;
		font-size: 12px;
	}
	.kw-input:focus,
	.name-input:focus,
	.tpl-input:focus {
		border-color: var(--fg-muted);
	}
	.kw-input::placeholder,
	.name-input::placeholder,
	.tpl-input::placeholder {
		color: var(--fg-muted);
		opacity: 0.5;
	}
	.form-error {
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--error);
	}
	.save-btn {
		display: flex;
		align-items: center;
		justify-content: center;
		gap: 4px;
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--fg-muted);
		background: none;
		border: 1px solid var(--border);
		border-radius: 6px;
		padding: 5px 10px;
		cursor: pointer;
	}
	.save-btn:hover {
		color: var(--fg);
		border-color: var(--fg-muted);
	}
	.preset-list {
		list-style: none;
		padding: 0;
		margin: 0;
	}
	.preset-item {
		display: flex;
		align-items: center;
		gap: 4px;
		border-bottom: 1px solid var(--border);
	}
	.preset-item:last-child {
		border-bottom: none;
	}
	.preset-content {
		flex: 1;
		display: flex;
		flex-direction: column;
		gap: 1px;
		padding: 7px 0;
		min-width: 0;
	}
	.preset-kw {
		display: inline-flex;
		align-items: center;
		gap: 5px;
		font-family: var(--font-mono);
		font-size: 13px;
		font-weight: 600;
		color: var(--accent);
	}
	.preset-name {
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--fg);
	}
	.preset-preview {
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--fg-muted);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.preset-edit {
		font-family: var(--font-mono);
		font-size: 10px;
		color: var(--fg-muted);
		background: none;
		border: none;
		cursor: pointer;
		padding: 2px 4px;
		opacity: 0;
		transition: opacity 100ms ease, color 100ms ease;
	}
	.preset-item:hover .preset-edit {
		opacity: 1;
	}
	.preset-edit:hover {
		color: var(--accent);
	}
	.preset-delete {
		display: flex;
		align-items: center;
		justify-content: center;
		background: none;
		border: none;
		color: var(--fg-muted);
		cursor: pointer;
		padding: 2px;
		border-radius: 3px;
		opacity: 0;
		transition: opacity 100ms ease, color 100ms ease;
		flex-shrink: 0;
	}
	.preset-item:hover .preset-delete {
		opacity: 1;
	}
	.preset-delete:hover {
		color: var(--error);
	}
	.preset-empty {
		font-family: var(--font-mono);
		font-size: 12px;
		color: var(--fg-muted);
		text-align: center;
		padding: 16px 0;
	}
</style>

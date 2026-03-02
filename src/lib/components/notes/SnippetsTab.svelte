<script lang="ts">
import { Plus, X } from "lucide-svelte";
import type { SnippetItem } from "$lib/ipc";
import { addSnippet, deleteSnippet, executeCommand, updateSnippet } from "$lib/ipc";

let {
	snippets = $bindable(),
}: {
	snippets: SnippetItem[];
} = $props();

let snippetName = $state("");
let snippetBody = $state("");
let editingSnippet: SnippetItem | null = $state(null);
let snippetCopied: string | null = $state(null);

async function handleAddSnippet() {
	const name = snippetName.trim();
	const body = snippetBody.trim();
	if (!name || !body) return;
	try {
		const item = await addSnippet(name, body);
		snippets = [...snippets, item];
		snippetName = "";
		snippetBody = "";
	} catch (err) {
		console.error("[snippets] add error:", err);
	}
}

async function handleDeleteSnippet(id: string) {
	try {
		await deleteSnippet(id);
		snippets = snippets.filter((s) => s.id !== id);
		if (editingSnippet?.id === id) editingSnippet = null;
	} catch (err) {
		console.error("[snippets] delete error:", err);
	}
}

function startEditSnippet(s: SnippetItem) {
	editingSnippet = s;
	snippetName = s.name;
	snippetBody = s.body;
}

async function handleSaveEditSnippet() {
	if (!editingSnippet) return;
	const name = snippetName.trim();
	const body = snippetBody.trim();
	if (!name || !body) return;
	try {
		await updateSnippet(editingSnippet.id, name, body);
		snippets = snippets.map((s) =>
			s.id === editingSnippet?.id ? { ...s, name, body, updated_at: Date.now() } : s,
		);
		editingSnippet = null;
		snippetName = "";
		snippetBody = "";
	} catch (err) {
		console.error("[snippets] update error:", err);
	}
}

function cancelEditSnippet() {
	editingSnippet = null;
	snippetName = "";
	snippetBody = "";
}

async function copySnippetBody(s: SnippetItem) {
	try {
		await navigator.clipboard.writeText(s.body);
		snippetCopied = s.id;
		setTimeout(() => {
			snippetCopied = null;
		}, 1500);
	} catch {
		await executeCommand(`snip ${s.name}`);
		snippetCopied = s.id;
		setTimeout(() => {
			snippetCopied = null;
		}, 1500);
	}
}
</script>

<div class="section">
	{#if editingSnippet}
		<div class="snippet-form">
			<div class="snippet-form-header">
				<span class="snippet-form-title">Edit snippet</span>
				<button class="snippet-cancel" onclick={cancelEditSnippet} onmousedown={(e) => e.preventDefault()} tabindex={-1}>Cancel</button>
			</div>
			<input
				class="snippet-name-input"
				bind:value={snippetName}
				placeholder="Name"
				type="text"
				maxlength={40}
			/>
			<textarea
				class="snippet-body-input"
				bind:value={snippetBody}
				placeholder="Snippet body..."
				maxlength={5000}
				rows={4}
			></textarea>
			<button class="snippet-save-btn" onclick={handleSaveEditSnippet} onmousedown={(e) => e.preventDefault()} tabindex={-1}>Save</button>
		</div>
	{:else}
		<div class="snippet-form">
			<input
				class="snippet-name-input"
				bind:value={snippetName}
				onkeydown={(e) => { if (e.key === "Enter") { e.preventDefault(); e.stopPropagation(); } }}
				placeholder="Name (e.g. email-intro)"
				type="text"
				maxlength={40}
			/>
			<textarea
				class="snippet-body-input"
				bind:value={snippetBody}
				onkeydown={(e) => { if (e.key === "Enter" && e.ctrlKey) { e.preventDefault(); e.stopPropagation(); handleAddSnippet(); } }}
				placeholder="Snippet body... (Ctrl+Enter to save)"
				maxlength={5000}
				rows={3}
			></textarea>
			<button class="snippet-save-btn" onclick={handleAddSnippet} onmousedown={(e) => e.preventDefault()} tabindex={-1}>
				<Plus size={12} strokeWidth={1.5} /> Add snippet
			</button>
		</div>
		{#if snippets.length > 0}
			<ul class="snippet-list" role="list">
				{#each snippets as s (s.id)}
					<li class="snippet-item">
						<button
							class="snippet-content"
							onclick={() => copySnippetBody(s)}
							onmousedown={(e) => e.preventDefault()}
							tabindex={-1}
							title="Click to copy"
						>
							<span class="snippet-item-name">{s.name}</span>
							<span class="snippet-item-preview">{s.body.split("\n")[0]?.slice(0, 50) || ""}</span>
							{#if snippetCopied === s.id}
								<span class="snippet-copied">Copied</span>
							{/if}
						</button>
						<button
							class="snippet-edit"
							onclick={() => startEditSnippet(s)}
							onmousedown={(e) => e.preventDefault()}
							tabindex={-1}
							aria-label="Edit"
						>Edit</button>
						<button
							class="snippet-delete"
							onclick={() => handleDeleteSnippet(s.id)}
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
			<div class="snippet-empty">No snippets. Add one above or use <code>snip add name body...</code></div>
		{/if}
	{/if}
</div>

<style>
	.section {
		padding: 10px 20px;
	}

	.snippet-form {
		display: flex;
		flex-direction: column;
		gap: 6px;
		margin-bottom: 8px;
	}

	.snippet-form-header {
		display: flex;
		justify-content: space-between;
		align-items: center;
	}

	.snippet-form-title {
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--fg-muted);
		text-transform: uppercase;
		letter-spacing: 0.3px;
	}

	.snippet-cancel {
		font-family: var(--font-mono);
		font-size: 10px;
		color: var(--fg-muted);
		background: none;
		border: none;
		cursor: pointer;
		padding: 2px 6px;
		transition: color 100ms ease;
	}

	.snippet-cancel:hover {
		color: var(--fg);
	}

	.snippet-name-input {
		width: 100%;
		background: var(--bg-secondary);
		border: 1px solid var(--border);
		border-radius: 6px;
		color: var(--fg);
		font-family: var(--font-mono);
		font-size: 13px;
		padding: 6px 10px;
		outline: none;
	}

	.snippet-name-input:focus {
		border-color: var(--fg-muted);
	}

	.snippet-name-input::placeholder {
		color: var(--fg-muted);
		opacity: 0.5;
	}

	.snippet-body-input {
		width: 100%;
		background: var(--bg-secondary);
		border: 1px solid var(--border);
		border-radius: 6px;
		color: var(--fg);
		font-family: var(--font-mono);
		font-size: 12px;
		padding: 6px 10px;
		resize: none;
		outline: none;
		line-height: 1.5;
	}

	.snippet-body-input:focus {
		border-color: var(--fg-muted);
	}

	.snippet-body-input::placeholder {
		color: var(--fg-muted);
		opacity: 0.5;
	}

	.snippet-save-btn {
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
		transition: color 100ms ease, border-color 100ms ease;
	}

	.snippet-save-btn:hover {
		color: var(--fg);
		border-color: var(--fg-muted);
	}

	.snippet-list {
		list-style: none;
		padding: 0;
		margin: 0;
	}

	.snippet-item {
		display: flex;
		align-items: center;
		gap: 4px;
		border-bottom: 1px solid var(--border);
	}

	.snippet-item:last-child {
		border-bottom: none;
	}

	.snippet-content {
		flex: 1;
		display: flex;
		flex-direction: column;
		gap: 1px;
		background: none;
		border: none;
		padding: 6px 0;
		cursor: pointer;
		text-align: left;
		min-width: 0;
		position: relative;
	}

	.snippet-item-name {
		font-family: var(--font-mono);
		font-size: 13px;
		font-weight: 600;
		color: var(--fg);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.snippet-content:hover .snippet-item-name {
		color: var(--accent);
	}

	.snippet-item-preview {
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--fg-muted);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.snippet-copied {
		position: absolute;
		right: 0;
		top: 50%;
		transform: translateY(-50%);
		font-family: var(--font-mono);
		font-size: 10px;
		color: var(--success, #4caf50);
		background: var(--bg);
		padding: 2px 6px;
		border-radius: 3px;
	}

	.snippet-edit {
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

	.snippet-item:hover .snippet-edit {
		opacity: 1;
	}

	.snippet-edit:hover {
		color: var(--accent);
	}

	.snippet-delete {
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

	.snippet-item:hover .snippet-delete {
		opacity: 1;
	}

	.snippet-delete:hover {
		color: var(--error);
	}

	.snippet-empty {
		font-family: var(--font-mono);
		font-size: 12px;
		color: var(--fg-muted);
		text-align: center;
		padding: 16px 0;
	}

	.snippet-empty code {
		color: var(--fg);
		background: var(--bg-secondary);
		padding: 1px 4px;
		border-radius: 3px;
		font-size: 11px;
	}
</style>

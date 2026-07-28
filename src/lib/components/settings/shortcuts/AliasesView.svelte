<script lang="ts">
import { Check, Pencil, X } from "lucide-svelte";
import type { AliasItem } from "$lib/ipc";
import { addAlias, deleteAlias, getAliases, getReservedKeywords, updateAlias } from "$lib/ipc";

let {
	onsaveerror,
	oncount,
}: {
	onsaveerror: (msg: string) => void;
	oncount?: (n: number) => void;
} = $props();

// Aliases live in the database, not in config.toml, so this view owns its own
// list rather than binding to a config prop.
let aliases = $state<AliasItem[]>([]);
let error = $state("");

let rows = $derived([...aliases].sort((a, b) => a.name.localeCompare(b.name)));

$effect(() => {
	oncount?.(rows.length);
});

// Same reserved-keyword source as quicklinks: the live action registry. An alias
// that shadows a built-in would silently shadow it, so it is refused here too.
let reservedKeywords = $state<Set<string>>(new Set());

async function refresh() {
	try {
		aliases = await getAliases();
	} catch (err) {
		onsaveerror(`Failed to load aliases: ${err}`);
	}
}

$effect(() => {
	refresh();
	getReservedKeywords()
		.then((words) => {
			reservedKeywords = new Set(words);
		})
		.catch(() => {
			// The backend still validates on save.
		});
});

let newName = $state("");
let newCommand = $state("");

function validateName(raw: string, { allow }: { allow?: string } = {}): string {
	const name = raw.trim().toLowerCase();
	if (!name) return "Alias can't be empty";
	if (/\s/.test(name)) return "Alias must be a single word (no spaces)";
	if (reservedKeywords.has(name)) return `"${name}" is a built-in command and can't be used`;
	if (name !== allow && rows.some((a) => a.name.toLowerCase() === name)) {
		return `"${name}" already exists`;
	}
	return "";
}

async function add() {
	const name = newName.trim().toLowerCase();
	const command = newCommand.trim();
	const nameErr = validateName(name);
	if (nameErr) {
		error = nameErr;
		return;
	}
	if (!command) {
		error = "Command can't be empty";
		return;
	}
	try {
		await addAlias(name, command);
		await refresh();
		newName = "";
		newCommand = "";
		error = "";
	} catch (err) {
		error = String(err);
	}
}

async function remove(name: string) {
	try {
		await deleteAlias(name);
		await refresh();
	} catch (err) {
		error = String(err);
	}
}

// --- Inline editing ---
let editingName = $state<string | null>(null);
let editCommand = $state("");

function startEdit(alias: AliasItem) {
	editingName = alias.name;
	editCommand = alias.command;
	error = "";
}

function cancelEdit() {
	editingName = null;
	error = "";
}

async function saveEdit(name: string) {
	const command = editCommand.trim();
	if (!command) {
		error = "Command can't be empty";
		return;
	}
	try {
		// Only the command is editable. Renaming would be a delete + add, and
		// silently doing that would lose the alias if the add then failed.
		await updateAlias(name, command);
		await refresh();
		editingName = null;
		error = "";
	} catch (err) {
		error = String(err);
	}
}
</script>

<div class="section-label first">Aliases</div>
<div class="field-hint">
	A short name for a longer command — <code>gs</code> for <code>git status</code>.
	Anything you type after the alias is appended, so <code>gs --short</code> runs
	<code>git status --short</code>. For placing input <em>inside</em> a command, use
	a quicklink instead.
</div>

{#if rows.length > 0}
	<div class="entry-list">
		{#each rows as alias (alias.name)}
			{#if editingName === alias.name}
				<div class="entry-row">
					<span class="entry-keyword">{alias.name}</span>
					<input
						type="text"
						class="row-input value-input"
						bind:value={editCommand}
						spellcheck="false"
						placeholder="git status"
						oninput={() => (error = "")}
						onkeydown={(e) => {
							if (e.key === "Enter") { e.preventDefault(); saveEdit(alias.name); }
							else if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); cancelEdit(); }
						}}
					/>
					<button
						class="entry-icon-btn save"
						onclick={() => saveEdit(alias.name)}
						title="Save"
						aria-label="Save"
					>
						<Check size={13} strokeWidth={2} />
					</button>
					<button class="entry-icon-btn" onclick={cancelEdit} title="Cancel" aria-label="Cancel">
						<X size={13} strokeWidth={2} />
					</button>
				</div>
			{:else}
				<div class="entry-row">
					<span class="entry-keyword">{alias.name}</span>
					<span class="arrow">→</span>
					<span class="entry-value" title={alias.command}>{alias.command}</span>
					<button
						class="entry-icon-btn"
						onclick={() => startEdit(alias)}
						title="Edit {alias.name}"
						aria-label="Edit {alias.name}"
					>
						<Pencil size={12} strokeWidth={2} />
					</button>
					<button
						class="entry-icon-btn remove"
						onclick={() => remove(alias.name)}
						title="Remove {alias.name}"
						aria-label="Remove {alias.name}"
					>
						<X size={13} strokeWidth={2} />
					</button>
				</div>
			{/if}
		{/each}
	</div>
{:else}
	<div class="empty-state">
		No aliases yet. An alias is a shorthand for a command you type often —
		<code>gs</code> for <code>git status</code>, or <code>ll</code> for <code>ls -la</code>.
	</div>
{/if}

<div class="entry-add">
	<input
		type="text"
		class="row-input kw-input"
		bind:value={newName}
		spellcheck="false"
		placeholder="alias"
		oninput={() => (error = "")}
		onkeydown={(e) => { if (e.key === "Enter") { e.preventDefault(); add(); } }}
	/>
	<input
		type="text"
		class="row-input value-input"
		bind:value={newCommand}
		spellcheck="false"
		placeholder="git status"
		oninput={() => (error = "")}
		onkeydown={(e) => { if (e.key === "Enter") { e.preventDefault(); add(); } }}
	/>
	<button class="add-btn" onclick={add}>Add</button>
</div>

{#if error}
	<div class="field-error">{error}</div>
{/if}

<style>
	@import "./rows.css";

	.arrow {
		color: var(--fg-muted);
		font-size: 11px;
		flex-shrink: 0;
		opacity: 0.7;
	}
</style>

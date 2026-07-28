<script lang="ts">
import { Check, Pencil, X } from "lucide-svelte";
import type { CommandsConfig, Quicklink } from "$lib/ipc";
import { getReservedKeywords, saveCommandsConfig } from "$lib/ipc";
import { invalidateSettings } from "$lib/preloadCache";

let {
	commandsConfig = $bindable(),
	onsaveerror,
	oncount,
}: {
	commandsConfig: CommandsConfig;
	onsaveerror: (msg: string) => void;
	oncount?: (n: number) => void;
} = $props();

// The reserved-keyword list comes from the backend's live action registry — the
// same source `validate_quicklinks` checks on save. Keeping a copy here would be
// a second decider that silently drifts every time a handler is added.
let reservedKeywords = $state<Set<string>>(new Set());
$effect(() => {
	// Fire-and-forget: the list only drives an instant-feedback warning, and the
	// backend still rejects a colliding save. Blocking the panel on it would cost
	// first paint for a hint.
	getReservedKeywords()
		.then((words) => {
			reservedKeywords = new Set(words);
		})
		.catch(() => {
			// Offline/failed: the save-path check still protects correctness.
		});
});

const KINDS = [
	{ value: "url", label: "Open URL", placeholder: "https://example.com/search?q={query}" },
	{ value: "shell", label: "Run command", placeholder: "git checkout {branch}" },
	{ value: "open", label: "Open path", placeholder: "~/projects/{name}" },
	{ value: "command", label: "Lychi command", placeholder: "note add {text}" },
] as const;

// `kind` is optional on the wire (it has a serde default), but every code path
// here needs a concrete value. Resolving it once at the boundary keeps the
// fallback in one place instead of a `?? "url"` at each use.
type Kind = NonNullable<Quicklink["kind"]>;
const kindOf = (link: Quicklink): Kind => link.kind ?? "url";
const labelOf = (kind: Kind) => KINDS.find((k) => k.value === kind)?.label ?? kind;

let rows = $derived(
	[...(commandsConfig.quicklinks ?? [])].sort((a, b) => a.keyword.localeCompare(b.keyword)),
);

$effect(() => {
	oncount?.(rows.length);
});

let newKeyword = $state("");
let newKind = $state<Kind>("url");
let newTemplate = $state("");
let error = $state("");

let newPlaceholder = $derived(KINDS.find((k) => k.value === newKind)?.placeholder ?? "");

/** Preview what a template expands to, so a shell quicklink shows the real
 *  command shape before it is ever run. */
function previewOf(template: string, kind: string): string {
	if (!template.trim()) return "";
	const sample = kind === "url" ? "search terms" : "value";
	return template.replace(/\{[a-zA-Z0-9_-]*\}/g, sample);
}

function validateKeyword(raw: string): string {
	const kw = raw.trim().toLowerCase();
	if (!kw) return "Keyword can't be empty";
	if (/\s/.test(kw)) return "Keyword must be a single word (no spaces)";
	if (reservedKeywords.has(kw)) return `"${kw}" is a reserved command and can't be used`;
	return "";
}

function validateTemplate(template: string, kind: string): string {
	const t = template.trim();
	if (!t) return "Template can't be empty";
	// Mirrors the backend's parse: an unclosed brace is rejected up front rather
	// than failing at run time.
	const opens = (t.match(/\{/g) ?? []).length;
	const closes = (t.match(/\}/g) ?? []).length;
	if (opens !== closes) return "Template has an unclosed { placeholder";
	if (kind === "url" && !/^https?:\/\//i.test(t)) {
		return "A URL quicklink must start with http:// or https://";
	}
	return "";
}

async function persist() {
	try {
		await saveCommandsConfig(commandsConfig);
		invalidateSettings();
		error = "";
	} catch (err) {
		error = String(err);
		onsaveerror(`Failed to save: ${err}`);
	}
}

async function add() {
	const kw = newKeyword.trim().toLowerCase();
	const template = newTemplate.trim();
	const kwErr = validateKeyword(kw);
	if (kwErr) {
		error = kwErr;
		return;
	}
	const tplErr = validateTemplate(template, newKind);
	if (tplErr) {
		error = tplErr;
		return;
	}
	if (rows.some((q) => q.keyword.toLowerCase() === kw)) {
		error = `"${kw}" already exists — edit it instead`;
		return;
	}
	commandsConfig.quicklinks = [
		...(commandsConfig.quicklinks ?? []),
		{ keyword: kw, name: "", kind: newKind, template },
	];
	await persist();
	newKeyword = "";
	newTemplate = "";
	newKind = "url";
}

async function remove(keyword: string) {
	commandsConfig.quicklinks = (commandsConfig.quicklinks ?? []).filter(
		(q) => q.keyword !== keyword,
	);
	await persist();
}

// --- Inline editing ---
let editingKeyword = $state<string | null>(null);
let editKeyword = $state("");
let editKind = $state<Kind>("url");
let editTemplate = $state("");

let editPlaceholder = $derived(KINDS.find((k) => k.value === editKind)?.placeholder ?? "");

function startEdit(link: Quicklink) {
	editingKeyword = link.keyword;
	editKeyword = link.keyword;
	editKind = kindOf(link);
	editTemplate = link.template;
	error = "";
}

function cancelEdit() {
	editingKeyword = null;
	error = "";
}

async function saveEdit(originalKeyword: string) {
	const kw = editKeyword.trim().toLowerCase();
	const template = editTemplate.trim();
	const kwErr = validateKeyword(kw);
	if (kwErr) {
		error = kwErr;
		return;
	}
	const tplErr = validateTemplate(template, editKind);
	if (tplErr) {
		error = tplErr;
		return;
	}
	// Changing the keyword is a rename — reject a collision with a *different*
	// existing quicklink (keeping the same key it already has is fine).
	if (kw !== originalKeyword && rows.some((q) => q.keyword.toLowerCase() === kw)) {
		error = `"${kw}" already exists`;
		return;
	}
	commandsConfig.quicklinks = (commandsConfig.quicklinks ?? []).map((q) =>
		q.keyword === originalKeyword
			? { keyword: kw, name: q.name ?? "", kind: editKind, template }
			: q,
	);
	await persist();
	editingKeyword = null;
}
</script>

<div class="section-label first">Quicklinks</div>
<div class="field-hint">
	Type a keyword then your input — e.g. <code>gh tokio</code>. Use
	<code>{"{name}"}</code> in the template to place the input; with no placeholder
	it's appended. Several placeholders split the input by word, and the last one
	takes the rest.
</div>

{#if rows.length > 0}
	<div class="entry-list">
		{#each rows as link (link.keyword)}
			{#if editingKeyword === link.keyword}
				<div class="entry-row">
					<input
						type="text"
						class="row-input kw-input"
						bind:value={editKeyword}
						spellcheck="false"
						placeholder="keyword"
						oninput={() => (error = "")}
						onkeydown={(e) => {
							if (e.key === "Enter") { e.preventDefault(); saveEdit(link.keyword); }
							else if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); cancelEdit(); }
						}}
					/>
					<select class="kind-select" bind:value={editKind} oninput={() => (error = "")}>
						{#each KINDS as k (k.value)}
							<option value={k.value}>{k.label}</option>
						{/each}
					</select>
					<input
						type="text"
						class="row-input value-input"
						bind:value={editTemplate}
						spellcheck="false"
						placeholder={editPlaceholder}
						oninput={() => (error = "")}
						onkeydown={(e) => {
							if (e.key === "Enter") { e.preventDefault(); saveEdit(link.keyword); }
							else if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); cancelEdit(); }
						}}
					/>
					<button
						class="entry-icon-btn save"
						onclick={() => saveEdit(link.keyword)}
						title="Save"
						aria-label="Save"
					>
						<Check size={13} strokeWidth={2} />
					</button>
					<button class="entry-icon-btn" onclick={cancelEdit} title="Cancel" aria-label="Cancel">
						<X size={13} strokeWidth={2} />
					</button>
				</div>
				{#if editKind === "shell"}
					<div class="note">Runs a shell command. Destructive commands still ask first.</div>
				{/if}
			{:else}
				<div class="entry-row">
					<span class="entry-keyword">{link.keyword}</span>
					<span class="kind-tag">{labelOf(kindOf(link))}</span>
					<span class="entry-value" title={link.template}>{link.template}</span>
					<button
						class="entry-icon-btn"
						onclick={() => startEdit(link)}
						title="Edit {link.keyword}"
						aria-label="Edit {link.keyword}"
					>
						<Pencil size={12} strokeWidth={2} />
					</button>
					<button
						class="entry-icon-btn remove"
						onclick={() => remove(link.keyword)}
						title="Remove {link.keyword}"
						aria-label="Remove {link.keyword}"
					>
						<X size={13} strokeWidth={2} />
					</button>
				</div>
			{/if}
		{/each}
	</div>
{:else}
	<div class="empty-state">
		No quicklinks yet. A quicklink turns a keyword into a parameterized action —
		<code>gh tokio</code> to search GitHub, or <code>co main</code> to check out a branch.
	</div>
{/if}

<div class="entry-add">
	<input
		type="text"
		class="row-input kw-input"
		bind:value={newKeyword}
		spellcheck="false"
		placeholder="keyword"
		oninput={() => (error = "")}
		onkeydown={(e) => { if (e.key === "Enter") { e.preventDefault(); add(); } }}
	/>
	<select class="kind-select" bind:value={newKind} oninput={() => (error = "")}>
		{#each KINDS as k (k.value)}
			<option value={k.value}>{k.label}</option>
		{/each}
	</select>
	<input
		type="text"
		class="row-input value-input"
		bind:value={newTemplate}
		spellcheck="false"
		placeholder={newPlaceholder}
		oninput={() => (error = "")}
		onkeydown={(e) => { if (e.key === "Enter") { e.preventDefault(); add(); } }}
	/>
	<button class="add-btn" onclick={add}>Add</button>
</div>

{#if newTemplate.trim()}
	<div class="field-hint preview">
		<code>{newKeyword.trim() || "keyword"}</code> → <code>{previewOf(newTemplate, newKind)}</code>
	</div>
{/if}
{#if newKind === "shell" && newTemplate.trim()}
	<div class="note">
		Runs a shell command. Your input is quoted so it can't add extra commands, and
		destructive commands still ask before running.
	</div>
{/if}
{#if error}
	<div class="field-error">{error}</div>
{/if}

<style>
	@import "./rows.css";

	/* Kind picker — same visual weight as the inputs it sits between, so the row
	   still reads as one control strip rather than three designs. */
	.kind-select {
		flex: 0 0 108px;
		background: var(--bg-secondary);
		color: var(--fg);
		border: 1px solid var(--border);
		border-radius: 4px;
		padding: 5px 6px;
		font-family: var(--font-mono);
		font-size: 11px;
		cursor: pointer;
	}

	/* The kind on a saved row: quieter than the keyword, since the keyword is what
	   the user types and the kind is only context. */
	.kind-tag {
		font-size: 10px;
		color: var(--fg-muted);
		border: 1px solid var(--border);
		border-radius: 3px;
		padding: 1px 5px;
		flex-shrink: 0;
		white-space: nowrap;
	}

	.preview code {
		font-family: var(--font-mono);
		color: var(--fg-muted);
	}

	/* Shown when a quicklink will reach a shell. Informational, not an error — the
	   command is still gated, so this sets expectations rather than warning of a
	   fault. */
	.note {
		font-size: 11px;
		color: var(--fg-muted);
		padding: 2px 0 4px 0;
	}
</style>

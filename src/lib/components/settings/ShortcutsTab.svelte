<script lang="ts">
import { Check, Pencil, RotateCcw, X } from "lucide-svelte";
import type { CommandsConfig, KeybindingsConfig } from "$lib/ipc";
import { KEYBINDINGS_DEFAULTS, saveCommandsConfig, saveKeybindingsConfig } from "$lib/ipc";
import {
	ACTION_LABELS,
	type ActionId,
	ALL_ACTIONS,
	comboFromEvent,
	findConflicts,
	loadKeybindings,
} from "$lib/keybindings";
import { invalidateSettings } from "$lib/preloadCache";

let {
	keybindingsConfig = $bindable(),
	commandsConfig = $bindable(),
	onsaveerror,
}: {
	keybindingsConfig: KeybindingsConfig;
	commandsConfig: CommandsConfig;
	onsaveerror: (msg: string) => void;
} = $props();

let recordingAction: ActionId | null = $state(null);
let conflictWarning = $state("");

// --- Custom search-engine shortcuts ("bangs") ---
// Reserved command prefixes a shortcut keyword must not collide with. Mirrors
// is_known_prefix() on the backend — the backend is the source of truth and
// rejects a colliding save, but checking here gives instant feedback.
const RESERVED_KEYWORDS = new Set([
	"ask",
	"bm",
	"bookmark",
	"browse",
	"clip",
	"clipboard",
	"clear",
	"ctx",
	"close",
	"emoji",
	"focus",
	"kill",
	"appctl",
	"open",
	"pin",
	"unpin",
	"sym",
	"unicode",
	"web",
	"yt",
	"run",
	"calc",
	"calculator",
	"define",
	"file",
	"url",
	"media",
	"project",
	"quit",
	"system",
	"note",
	"notes",
	"todo",
	"todos",
	"snip",
	"snippet",
	"snippets",
	"weather",
	"sysinfo",
	"ip",
	"cpu",
	"mem",
	"disk",
	"temp",
	"gpu",
	"battery",
	"net",
	"audio",
	"display",
	"os",
	"speedtest",
	"time",
	"tz",
	"clock",
	"alias",
	"aliases",
	"timer",
	"stopwatch",
	"base64",
	"hash",
	"urlencode",
	"urldecode",
	"epoch",
	"qr",
	"resize",
	"json",
	"upper",
	"lower",
	"title",
	"slug",
	"reverse",
	"count",
	"random",
	"rand",
]);

let engineRows = $derived(
	Object.entries(commandsConfig.search_engines ?? {}).sort((a, b) => a[0].localeCompare(b[0])),
);

let newEngineKeyword = $state("");
let newEngineUrl = $state("");
let engineError = $state("");

function validateKeyword(raw: string): string {
	const kw = raw.trim().toLowerCase();
	if (!kw) return "Keyword can't be empty";
	if (/\s/.test(kw)) return "Keyword must be a single word (no spaces)";
	if (RESERVED_KEYWORDS.has(kw)) return `"${kw}" is a reserved command and can't be used`;
	return "";
}

async function persistEngines() {
	try {
		await saveCommandsConfig(commandsConfig);
		invalidateSettings();
		engineError = "";
	} catch (err) {
		engineError = String(err);
		onsaveerror(`Failed to save: ${err}`);
	}
}

async function addEngine() {
	const kw = newEngineKeyword.trim().toLowerCase();
	const url = newEngineUrl.trim();
	const kwErr = validateKeyword(kw);
	if (kwErr) {
		engineError = kwErr;
		return;
	}
	if (!/^https?:\/\//i.test(url)) {
		engineError = "URL must start with http:// or https://";
		return;
	}
	if (commandsConfig.search_engines?.[kw]) {
		engineError = `"${kw}" already exists — remove it first to change the URL`;
		return;
	}
	commandsConfig.search_engines = { ...(commandsConfig.search_engines ?? {}), [kw]: url };
	await persistEngines();
	newEngineKeyword = "";
	newEngineUrl = "";
}

async function removeEngine(keyword: string) {
	const next = { ...(commandsConfig.search_engines ?? {}) };
	delete next[keyword];
	commandsConfig.search_engines = next;
	await persistEngines();
}

// --- Inline editing of an existing shortcut ---
// The keyword being edited (null = not editing), plus draft fields for its row.
let editingKeyword = $state<string | null>(null);
let editKeyword = $state("");
let editUrl = $state("");

function startEdit(keyword: string, url: string) {
	editingKeyword = keyword;
	editKeyword = keyword;
	editUrl = url;
	engineError = "";
}

function cancelEdit() {
	editingKeyword = null;
	engineError = "";
}

async function saveEdit(originalKeyword: string) {
	const kw = editKeyword.trim().toLowerCase();
	const url = editUrl.trim();
	const kwErr = validateKeyword(kw);
	if (kwErr) {
		engineError = kwErr;
		return;
	}
	if (!/^https?:\/\//i.test(url)) {
		engineError = "URL must start with http:// or https://";
		return;
	}
	// Changing the keyword is a rename — reject a collision with a *different*
	// existing shortcut (renaming to the same key it already has is fine).
	if (kw !== originalKeyword && commandsConfig.search_engines?.[kw]) {
		engineError = `"${kw}" already exists`;
		return;
	}
	// Rebuild the map: drop the old key, set the (possibly renamed) new one.
	const next = { ...(commandsConfig.search_engines ?? {}) };
	delete next[originalKeyword];
	next[kw] = url;
	commandsConfig.search_engines = next;
	await persistEngines();
	editingKeyword = null;
}

function startRecording(action: ActionId) {
	recordingAction = action;
	conflictWarning = "";

	const handler = (e: KeyboardEvent) => {
		e.preventDefault();
		e.stopPropagation();

		const combo = comboFromEvent(e);
		if (!combo) return;

		if (e.key === "Escape" && !e.ctrlKey && !e.shiftKey && !e.altKey) {
			recordingAction = null;
			window.removeEventListener("keydown", handler, true);
			return;
		}

		const testConfig = { ...keybindingsConfig, [action]: combo };
		const conflicts = findConflicts(testConfig);
		if (conflicts.length > 0) {
			const [a, b] = conflicts[0];
			const other = a === action ? b : a;
			conflictWarning = `"${combo}" conflicts with ${ACTION_LABELS[other]}`;
		} else {
			conflictWarning = "";
		}

		keybindingsConfig = { ...keybindingsConfig, [action]: combo };
		loadKeybindings(keybindingsConfig);
		saveKeybindingsConfig(keybindingsConfig);
		invalidateSettings();
		recordingAction = null;
		window.removeEventListener("keydown", handler, true);
	};

	window.addEventListener("keydown", handler, true);
}

async function resetAllShortcuts() {
	keybindingsConfig = { ...KEYBINDINGS_DEFAULTS };
	loadKeybindings(keybindingsConfig);
	await saveKeybindingsConfig(keybindingsConfig);
	invalidateSettings();
	conflictWarning = "";
}
</script>

<div class="section-label">Keyboard Shortcuts</div>
{#each ALL_ACTIONS as action}
	<div class="field shortcut-row">
		<span class="field-label">{ACTION_LABELS[action]}</span>
		<button
			class="hotkey-btn"
			class:recording={recordingAction === action}
			onclick={() => startRecording(action)}
		>
			{#if recordingAction === action}
				Press keys...
			{:else}
				{keybindingsConfig[action]}
			{/if}
		</button>
	</div>
{/each}
{#if conflictWarning}
	<div class="field-error">{conflictWarning}</div>
{/if}
<div class="field" style="justify-content: flex-end; padding-top: 8px;">
	<button class="reset-btn" onclick={resetAllShortcuts}>
		<RotateCcw size={12} strokeWidth={1.5} />
		Reset all to defaults
	</button>
</div>

<div class="section-label">Search shortcuts</div>
<div class="field-hint">
	Type a keyword then a query to search a site directly — e.g. <code>gh tokio</code>.
	Use <code>{"{}"}</code> in the URL to place the query mid-path, otherwise it's appended.
</div>
{#if engineRows.length > 0}
	<div class="engine-list">
		{#each engineRows as [keyword, url] (keyword)}
			{#if editingKeyword === keyword}
				<div class="engine-row editing">
					<input
						type="text"
						class="engine-kw-input"
						bind:value={editKeyword}
						spellcheck="false"
						placeholder="keyword"
						oninput={() => (engineError = "")}
						onkeydown={(e) => {
							if (e.key === "Enter") { e.preventDefault(); saveEdit(keyword); }
							else if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); cancelEdit(); }
						}}
					/>
					<input
						type="text"
						class="engine-url-input"
						bind:value={editUrl}
						spellcheck="false"
						placeholder="https://example.com/search?q="
						oninput={() => (engineError = "")}
						onkeydown={(e) => {
							if (e.key === "Enter") { e.preventDefault(); saveEdit(keyword); }
							else if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); cancelEdit(); }
						}}
					/>
					<button
						class="engine-icon-btn save"
						onclick={() => saveEdit(keyword)}
						title="Save"
						aria-label="Save"
					>
						<Check size={13} strokeWidth={2} />
					</button>
					<button
						class="engine-icon-btn"
						onclick={cancelEdit}
						title="Cancel"
						aria-label="Cancel"
					>
						<X size={13} strokeWidth={2} />
					</button>
				</div>
			{:else}
				<div class="engine-row">
					<span class="engine-keyword">{keyword}</span>
					<span class="engine-url" title={url}>{url}</span>
					<button
						class="engine-icon-btn"
						onclick={() => startEdit(keyword, url ?? "")}
						title="Edit {keyword}"
						aria-label="Edit {keyword}"
					>
						<Pencil size={12} strokeWidth={2} />
					</button>
					<button
						class="engine-icon-btn remove"
						onclick={() => removeEngine(keyword)}
						title="Remove {keyword}"
						aria-label="Remove {keyword}"
					>
						<X size={13} strokeWidth={2} />
					</button>
				</div>
			{/if}
		{/each}
	</div>
{/if}
<div class="engine-add">
	<input
		type="text"
		class="engine-kw-input"
		bind:value={newEngineKeyword}
		spellcheck="false"
		placeholder="keyword"
		oninput={() => (engineError = "")}
		onkeydown={(e) => { if (e.key === "Enter") { e.preventDefault(); addEngine(); } }}
	/>
	<input
		type="text"
		class="engine-url-input"
		bind:value={newEngineUrl}
		spellcheck="false"
		placeholder="https://example.com/search?q="
		oninput={() => (engineError = "")}
		onkeydown={(e) => { if (e.key === "Enter") { e.preventDefault(); addEngine(); } }}
	/>
	<button class="add-btn" onclick={addEngine}>Add</button>
</div>
{#if engineError}
	<div class="field-error">{engineError}</div>
{/if}

<style>
	.section-label {
		font-size: 11px;
		color: var(--fg-muted);
		text-transform: uppercase;
		letter-spacing: 0.05em;
		padding: 12px 0 4px;
		border-top: 1px solid var(--border);
		margin-top: 4px;
	}

	.field {
		display: flex;
		align-items: center;
		justify-content: space-between;
		padding: 6px 0;
		gap: 12px;
	}

	.field-label {
		color: var(--fg-muted);
		font-size: 12px;
		flex-shrink: 0;
		width: 120px;
	}

	.field-error {
		font-size: 11px;
		color: var(--error);
		padding: 2px 0 4px 0;
	}

	.shortcut-row {
		display: flex;
		align-items: center;
		justify-content: space-between;
	}

	.shortcut-row .hotkey-btn {
		min-width: 110px;
	}

	.hotkey-btn {
		background: var(--bg-secondary);
		color: var(--fg);
		border: 1px solid var(--border);
		border-radius: 4px;
		padding: 5px 12px;
		font-family: var(--font-mono);
		font-size: 12px;
		min-width: 140px;
		text-align: center;
		cursor: pointer;
		transition: border-color 100ms ease;
	}

	.hotkey-btn:hover {
		border-color: var(--fg-muted);
	}

	.hotkey-btn.recording {
		border-color: var(--accent);
		color: var(--accent);
		animation: hotkey-pulse 1s ease-in-out infinite;
	}

	@keyframes hotkey-pulse {
		0%, 100% { opacity: 1; }
		50% { opacity: 0.4; }
	}

	.reset-btn {
		display: flex;
		align-items: center;
		gap: 5px;
		background: var(--bg-secondary);
		color: var(--fg-muted);
		border: 1px solid var(--border);
		border-radius: 4px;
		padding: 4px 10px;
		font-size: 11px;
		cursor: pointer;
		transition: color 100ms ease, border-color 100ms ease;
	}

	.reset-btn:hover {
		color: var(--fg);
		border-color: var(--fg-muted);
	}

	.field-hint {
		font-size: 10px;
		color: var(--fg-muted);
		opacity: 0.7;
		padding: 0 0 6px;
	}

	.field-hint code {
		font-family: var(--font-mono);
		background: var(--bg-secondary);
		padding: 1px 4px;
		border-radius: 3px;
		user-select: all;
	}

	input[type="text"] {
		background: var(--bg-secondary);
		color: var(--fg);
		border: 1px solid var(--border);
		border-radius: 4px;
		padding: 5px 8px;
		font-family: var(--font-mono);
		font-size: 12px;
		outline: none;
		min-width: 0;
	}

	input[type="text"]:focus {
		border-color: var(--fg-muted);
	}

	.engine-list {
		display: flex;
		flex-direction: column;
		gap: 4px;
		padding: 4px 0 8px;
	}

	.engine-row {
		display: flex;
		align-items: center;
		gap: 8px;
		min-width: 0;
	}

	.engine-keyword {
		font-family: var(--font-mono);
		font-size: 12px;
		color: var(--accent);
		background: var(--bg-secondary);
		border: 1px solid var(--border);
		border-radius: 3px;
		padding: 2px 6px;
		flex-shrink: 0;
		min-width: 48px;
		text-align: center;
	}

	.engine-url {
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--fg-muted);
		flex: 1;
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.engine-icon-btn {
		display: flex;
		align-items: center;
		justify-content: center;
		background: none;
		border: none;
		color: var(--fg-muted);
		cursor: pointer;
		padding: 3px;
		border-radius: 3px;
		flex-shrink: 0;
		transition: color 100ms ease, background 100ms ease;
	}

	.engine-icon-btn:hover {
		color: var(--fg);
		background: var(--bg-secondary);
	}

	.engine-icon-btn.remove:hover {
		color: var(--error);
	}

	.engine-icon-btn.save {
		color: var(--accent);
	}

	.engine-icon-btn.save:hover {
		color: var(--accent);
	}

	.engine-add {
		display: flex;
		gap: 6px;
		align-items: center;
		padding: 2px 0 4px;
	}

	.engine-kw-input {
		flex: 0 0 80px;
	}

	.engine-url-input {
		flex: 1;
	}

	.add-btn {
		background: var(--bg-secondary);
		color: var(--accent);
		border: 1px solid var(--border);
		border-radius: 4px;
		padding: 5px 12px;
		font-family: var(--font-mono);
		font-size: 12px;
		cursor: pointer;
		flex-shrink: 0;
		transition: background 100ms ease;
	}

	.add-btn:hover {
		background: var(--border);
	}
</style>

<script lang="ts">
import { getCurrentWindow } from "@tauri-apps/api/window";
import { Clipboard, Clock, StickyNote, Timer } from "lucide-svelte";
import { onMount } from "svelte";
import type { ReminderItem, ScratchItem, SnippetItem, TimerStatus } from "$lib/ipc";
import { getAllItems, getReminders, getSnippets, getTimers } from "$lib/ipc";
import { invalidateNotes } from "$lib/preloadCache";
import NotesTab from "./notes/NotesTab.svelte";
import RemindersTab from "./notes/RemindersTab.svelte";
import SnippetsTab from "./notes/SnippetsTab.svelte";
import TimersTab from "./notes/TimersTab.svelte";

let {
	ondismiss,
	pendingNoteText = null,
	onpendingcleared,
	initialNotesTab,
	visible = false,
}: {
	ondismiss: () => void;
	pendingNoteText?: string | null;
	onpendingcleared?: () => void;
	// "todos" accepted for backwards-compat with old sentinels → maps to notes.
	initialNotesTab?: "notes" | "todos" | "reminders" | "timers" | "snippets";
	visible?: boolean;
} = $props();

// Notes and Todos are now one unified "Notes" surface.
type TabId = "notes" | "reminders" | "timers" | "snippets";
let activeTab: TabId = $state("notes");
const TAB_ORDER: TabId[] = ["notes", "reminders", "timers", "snippets"];

// Data state — unified notes+todos list.
let items: ScratchItem[] = $state([]);
let reminders: ReminderItem[] = $state([]);
let timers: TimerStatus[] = $state([]);
let snippets: SnippetItem[] = $state([]);

// Derived for tab badges
let openChecklist = $derived(items.filter((i) => i.done === false).length);
let activeReminders = $derived(reminders.filter((r) => !r.fired));

let notesTabRef: NotesTab | undefined = $state();

// Switch tab when initialNotesTab changes from parent. The old "todos" tab is
// folded into "notes", so map it there for backwards-compat with any sentinel.
$effect(() => {
	if (initialNotesTab) {
		activeTab = initialNotesTab === "todos" ? "notes" : initialNotesTab;
	}
});

// Ctrl+Tab / Ctrl+Shift+Tab: cycle utility tabs (capture phase, only when visible)
$effect(() => {
	if (!visible) return;
	function onKeydown(e: KeyboardEvent) {
		if (e.ctrlKey && e.code === "Tab") {
			e.preventDefault();
			e.stopPropagation();
			const idx = TAB_ORDER.indexOf(activeTab);
			if (e.shiftKey) {
				activeTab = TAB_ORDER[(idx - 1 + TAB_ORDER.length) % TAB_ORDER.length];
			} else {
				activeTab = TAB_ORDER[(idx + 1) % TAB_ORDER.length];
			}
		}
	}
	window.addEventListener("keydown", onKeydown, true);
	return () => window.removeEventListener("keydown", onKeydown, true);
});

onMount(() => {
	initialLoad();

	let unlisten: (() => void) | undefined;
	(async () => {
		const win = getCurrentWindow();
		unlisten = await win.listen("lychi://notes-changed", () => {
			notesTabRef?.clearSaveTimer();
			invalidateNotes();
			reloadData();
		});
	})();

	return () => {
		unlisten?.();
	};
});

function initialLoad() {
	getAllItems()
		.then((i) => {
			requestAnimationFrame(() => {
				items = i;
			});
		})
		.catch((err) => {
			console.error("[notes] load error:", err);
		});
	getReminders()
		.then((r) => {
			reminders = r;
		})
		.catch(() => {});
	getSnippets()
		.then((s) => {
			snippets = s;
		})
		.catch(() => {});
}

async function reloadData() {
	try {
		const [i, r, s] = await Promise.all([getAllItems(), getReminders(), getSnippets()]);
		items = i;
		reminders = r;
		snippets = s;
	} catch (err) {
		console.error("[notes] load error:", err);
	}
}

function handleKeydown(e: KeyboardEvent) {
	const inEditor = notesTabRef?.isEditing();
	if (e.key === "Escape" || (e.key === "Enter" && inEditor)) {
		e.preventDefault();
		if (inEditor) {
			notesTabRef?.backToList();
		} else {
			notesTabRef?.clearSaveTimer();
			ondismiss();
		}
	}
}
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="notes-panel" onkeydown={handleKeydown}>
	<div class="tab-bar">
		<button
			class="tab"
			class:active={activeTab === "notes"}
			onmousedown={(e) => e.preventDefault()}
			onclick={() => { activeTab = "notes"; }}
			tabindex={-1}
		>
			<StickyNote size={11} strokeWidth={1.5} style="display:inline;vertical-align:-1px;margin-right:3px" />Notes{#if items.length > 0}<span class="tab-badge">{#if openChecklist > 0}{openChecklist}·{/if}{items.length}</span>{/if}
		</button>
		<button
			class="tab"
			class:active={activeTab === "reminders"}
			onmousedown={(e) => e.preventDefault()}
			onclick={() => { activeTab = "reminders"; getReminders().then((r) => { reminders = r; }).catch(() => {}); }}
			tabindex={-1}
		>
			<Clock size={11} strokeWidth={1.5} style="display:inline;vertical-align:-1px;margin-right:3px" />Reminders{#if activeReminders.length > 0}<span class="tab-badge">{activeReminders.length}</span>{/if}
		</button>
		<button
			class="tab"
			class:active={activeTab === "timers"}
			onmousedown={(e) => e.preventDefault()}
			onclick={() => { activeTab = "timers"; }}
			tabindex={-1}
		>
			<Timer size={11} strokeWidth={1.5} style="display:inline;vertical-align:-1px;margin-right:3px" />Timers{#if timers.length > 0}<span class="tab-badge">{timers.length}</span>{/if}
		</button>
		<button
			class="tab"
			class:active={activeTab === "snippets"}
			onmousedown={(e) => e.preventDefault()}
			onclick={() => { activeTab = "snippets"; }}
			tabindex={-1}
		>
			<Clipboard size={11} strokeWidth={1.5} style="display:inline;vertical-align:-1px;margin-right:3px" />Snippets{#if snippets.length > 0}<span class="tab-badge">{snippets.length}</span>{/if}
		</button>
	</div>

	{#if activeTab === "notes"}
		<NotesTab
			bind:this={notesTabRef}
			bind:items
			{pendingNoteText}
			{onpendingcleared}
		/>
	{:else if activeTab === "reminders"}
		<RemindersTab bind:reminders active={activeTab === "reminders"} />
	{:else if activeTab === "timers"}
		<TimersTab bind:timers active={activeTab === "timers"} />
	{:else if activeTab === "snippets"}
		<SnippetsTab bind:snippets />
	{/if}
</div>

<style>
	.notes-panel {
		overflow-y: auto;
		max-height: 340px;
	}

	.tab-bar {
		display: flex;
		padding: 0 20px;
		border-bottom: 1px solid var(--border);
	}

	.tab {
		font-family: var(--font-mono);
		font-size: 11px;
		text-transform: uppercase;
		letter-spacing: 0.5px;
		color: var(--fg-muted);
		background: none;
		border: none;
		border-bottom: 2px solid transparent;
		margin-bottom: -1px;
		padding: 8px 12px;
		cursor: pointer;
		outline: none;
		transition: color 100ms ease, border-color 100ms ease;
	}

	.tab:hover {
		color: var(--fg);
	}

	.tab.active {
		color: var(--fg);
		border-bottom-color: var(--accent);
	}

	.tab-badge {
		font-size: 10px;
		color: var(--fg-muted);
		margin-left: 4px;
	}

</style>

<script lang="ts">
import { getCurrentWindow } from "@tauri-apps/api/window";
import {
	ChevronLeft,
	Clipboard,
	Clock,
	ListChecks,
	Plus,
	StickyNote,
	Timer,
	X,
} from "lucide-svelte";
import { onMount } from "svelte";
import type { NoteItem, ReminderItem, SnippetItem, TimerStatus, TodoItem } from "$lib/ipc";
import {
	addNote,
	addSnippet,
	addTodo,
	deleteNote,
	deleteReminder,
	deleteSnippet,
	deleteTodo,
	executeCommand,
	getNotes,
	getReminders,
	getSnippets,
	getTimers,
	getTodos,
	toggleTodo,
	updateNote,
	updateSnippet,
} from "$lib/ipc";
import { invalidateNotes, preloadNotes } from "$lib/preloadCache";

const MAX_NOTES = 5;
const MAX_NOTE_CHARS = 500;

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
	initialNotesTab?: "notes" | "todos" | "reminders" | "timers" | "snippets";
	visible?: boolean;
} = $props();

type TabId = "notes" | "todos" | "reminders" | "timers" | "snippets";
let activeTab: TabId = $state("notes");

// Switch tab when initialNotesTab changes from parent
$effect(() => {
	if (initialNotesTab) {
		activeTab = initialNotesTab;
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

// Notes state
let notes: NoteItem[] = $state([]);
let editingNote: NoteItem | null = $state(null);
let isNewNote = $state(false);
let editText = $state("");
let saveTimer: ReturnType<typeof setTimeout> | undefined;

// Todos state
let todos: TodoItem[] = $state([]);
let todoInput = $state("");

// Reminders state
let reminders: ReminderItem[] = $state([]);
let reminderRefreshTimer: ReturnType<typeof setInterval> | undefined;
let reminderInput = $state("");

// Timer state
let timers: TimerStatus[] = $state([]);
let timerPollInterval: ReturnType<typeof setInterval> | undefined;
let timerInput = $state("");

// Snippets state
let snippets: SnippetItem[] = $state([]);
let snippetName = $state("");
let snippetBody = $state("");
let editingSnippet: SnippetItem | null = $state(null);
let snippetCopied: string | null = $state(null);

// Derived
let sortedTodos = $derived.by(() => {
	const unchecked = todos.filter((t) => !t.done);
	const checked = todos.filter((t) => t.done);
	return [...unchecked, ...checked];
});
let remainingTodos = $derived(todos.filter((t) => !t.done).length);

let activeReminders = $derived(reminders.filter((r) => !r.fired));
let firedReminders = $derived(reminders.filter((r) => r.fired));

onMount(() => {
	initialLoad();

	let unlisten: (() => void) | undefined;
	(async () => {
		const win = getCurrentWindow();
		unlisten = await win.listen("lychi://notes-changed", () => {
			clearTimeout(saveTimer);
			invalidateNotes();
			reloadData();
		});
	})();

	return () => {
		unlisten?.();
	};
});

function initialLoad() {
	preloadNotes()
		.then((cached) => {
			requestAnimationFrame(() => {
				notes = cached.notes;
				todos = cached.todos;
			});
		})
		.catch((err) => {
			console.error("[notes] load error:", err);
		});
	// Load reminders non-blocking
	getReminders()
		.then((r) => {
			reminders = r;
		})
		.catch(() => {});
	// Load snippets non-blocking
	getSnippets()
		.then((s) => {
			snippets = s;
		})
		.catch(() => {});
}

async function reloadData() {
	clearTimeout(saveTimer);
	try {
		const [n, t, r, s] = await Promise.all([getNotes(), getTodos(), getReminders(), getSnippets()]);
		notes = n;
		todos = t;
		reminders = r;
		snippets = s;
		if (editingNote && !n.find((x) => x.id === editingNote?.id)) {
			editingNote = null;
		}
	} catch (err) {
		console.error("[notes] load error:", err);
	}
}

// --- Note actions ---

function noteTitle(text: string): string {
	const first = text.split("\n")[0] || text;
	return first.length > 50 ? `${first.slice(0, 50)}…` : first;
}

function openNote(note: NoteItem) {
	editingNote = note;
	editText = note.text;
}

function handleNewNote() {
	isNewNote = true;
	editingNote = null;
	editText = "";
}

function handleNoteInput() {
	if (editText.length > MAX_NOTE_CHARS) {
		editText = editText.slice(0, MAX_NOTE_CHARS);
	}
	clearTimeout(saveTimer);
	saveTimer = setTimeout(saveCurrentNote, 500);
}

async function saveCurrentNote() {
	if (!editText.trim()) return;
	try {
		if (isNewNote) {
			const item = await addNote(editText);
			notes = [...notes, item];
			editingNote = item;
			isNewNote = false;
		} else if (editingNote) {
			await updateNote(editingNote.id, editText);
			notes = notes.map((n) =>
				n.id === editingNote?.id ? { ...n, text: editText, updated_at: Date.now() } : n,
			);
		}
	} catch (err) {
		console.error("[notes] save error:", err);
	}
}

async function backToList() {
	clearTimeout(saveTimer);
	if (editText.trim()) {
		await saveCurrentNote();
	} else if (editingNote && !isNewNote) {
		// Delete existing note if cleared to empty
		await handleDeleteNote(editingNote.id);
	}
	editingNote = null;
	isNewNote = false;
}

async function handleDeleteNote(id: string) {
	try {
		await deleteNote(id);
		notes = notes.filter((n) => n.id !== id);
		if (editingNote?.id === id) editingNote = null;

		// Auto-add pending note if there's room now
		if (pendingNoteText && notes.length < MAX_NOTES) {
			try {
				const item = await addNote(pendingNoteText);
				notes = [...notes, item];
				onpendingcleared?.();
			} catch (addErr) {
				console.error("[notes] auto-add pending error:", addErr);
			}
		}
	} catch (err) {
		console.error("[notes] delete error:", err);
	}
}

function cancelPending() {
	onpendingcleared?.();
}

// --- Todo actions ---

async function handleAddTodo() {
	const text = todoInput.trim();
	if (!text) return;
	try {
		const item = await addTodo(text);
		todos = [...todos, item];
		todoInput = "";
	} catch (err) {
		console.error("[notes] add todo error:", err);
	}
}

async function handleToggle(id: string) {
	try {
		await toggleTodo(id);
		todos = todos.map((t) => (t.id === id ? { ...t, done: !t.done } : t));
	} catch (err) {
		console.error("[notes] toggle error:", err);
	}
}

async function handleDeleteTodo(id: string) {
	try {
		await deleteTodo(id);
		todos = todos.filter((t) => t.id !== id);
	} catch (err) {
		console.error("[notes] delete error:", err);
	}
}

// --- Reminder actions ---

function formatReminderTime(dueAt: number): string {
	const now = Date.now();
	if (dueAt <= now) return "now";
	const diffSecs = Math.floor((dueAt - now) / 1000);
	if (diffSecs < 60) return `${diffSecs}s`;
	if (diffSecs < 3600) {
		const m = Math.floor(diffSecs / 60);
		const s = diffSecs % 60;
		return s > 0 ? `${m}m ${s}s` : `${m}m`;
	}
	if (diffSecs < 86400) {
		const h = Math.floor(diffSecs / 3600);
		const m = Math.floor((diffSecs % 3600) / 60);
		return m > 0 ? `${h}h ${m}m` : `${h}h`;
	}
	const d = Math.floor(diffSecs / 86400);
	const h = Math.floor((diffSecs % 86400) / 3600);
	return h > 0 ? `${d}d ${h}h` : `${d}d`;
}

function formatReminderDueLabel(dueAt: number): string {
	const date = new Date(dueAt);
	const now = new Date();
	const isToday = date.toDateString() === now.toDateString();
	const tomorrow = new Date(now);
	tomorrow.setDate(tomorrow.getDate() + 1);
	const isTomorrow = date.toDateString() === tomorrow.toDateString();
	const hours = date.getHours();
	const minutes = date.getMinutes();
	const ampm = hours >= 12 ? "pm" : "am";
	const h12 = hours % 12 || 12;
	const time =
		minutes > 0 ? `${h12}:${minutes.toString().padStart(2, "0")}${ampm}` : `${h12}${ampm}`;
	if (isToday) return `today ${time}`;
	if (isTomorrow) return `tomorrow ${time}`;
	const month = date.toLocaleString("en", { month: "short" }).toLowerCase();
	return `${month} ${date.getDate()} ${time}`;
}

function reminderProgress(rem: ReminderItem): number {
	const now = Date.now();
	if (rem.fired || now >= rem.due_at) return 1;
	const total = rem.due_at - rem.created_at;
	if (total <= 0) return 1;
	const elapsed = now - rem.created_at;
	return Math.max(0, Math.min(1, elapsed / total));
}

async function handleAddReminder() {
	const text = reminderInput.trim();
	if (!text) return;
	try {
		await executeCommand(`reminder add ${text}`);
		reminderInput = "";
		// Refresh reminders list
		reminders = await getReminders();
	} catch (err) {
		console.error("[reminders] add error:", err);
	}
}

async function handleDeleteReminder(id: string) {
	try {
		await deleteReminder(id);
		reminders = reminders.filter((r) => r.id !== id);
	} catch (err) {
		console.error("[reminders] delete error:", err);
	}
}

// Auto-refresh reminders countdown every 30s when tab is active
$effect(() => {
	if (activeTab === "reminders") {
		reminderRefreshTimer = setInterval(() => {
			getReminders()
				.then((r) => {
					reminders = r;
				})
				.catch(() => {});
		}, 10_000);
	} else {
		clearInterval(reminderRefreshTimer);
	}
	return () => clearInterval(reminderRefreshTimer);
});

// Poll timers every 100ms when timers tab is active
$effect(() => {
	if (activeTab === "timers") {
		async function poll() {
			try {
				timers = await getTimers();
			} catch {
				/* ignore */
			}
		}
		poll();
		timerPollInterval = setInterval(poll, 100);
	} else {
		clearInterval(timerPollInterval);
	}
	return () => clearInterval(timerPollInterval);
});

// --- Timer helpers ---

function formatTimerTime(secs: number): string {
	const total = Math.ceil(secs);
	if (total >= 3600) {
		const h = Math.floor(total / 3600);
		const m = Math.floor((total % 3600) / 60);
		const s = total % 60;
		return `${h}:${m.toString().padStart(2, "0")}:${s.toString().padStart(2, "0")}`;
	}
	const m = Math.floor(total / 60);
	const s = total % 60;
	return `${m}:${s.toString().padStart(2, "0")}`;
}

function formatTimerDuration(secs: number): string {
	const total = Math.ceil(secs);
	if (total >= 3600) {
		const h = Math.floor(total / 3600);
		const m = Math.floor((total % 3600) / 60);
		return m > 0 ? `${h}h ${m}m` : `${h}h`;
	}
	if (total >= 60) {
		const m = Math.floor(total / 60);
		const s = total % 60;
		return s > 0 ? `${m}m ${s}s` : `${m}m`;
	}
	return `${total}s`;
}

function timerProgress(t: TimerStatus): number {
	if (t.duration_secs <= 0) return 0;
	return Math.min(1, t.elapsed_secs / t.duration_secs);
}

async function handleCreateTimer() {
	const text = timerInput.trim();
	if (!text) return;
	try {
		await executeCommand(`timer ${text}`);
		timerInput = "";
	} catch (err) {
		console.error("[timer] create error:", err);
	}
}

async function handleTimerPause(name: string) {
	await executeCommand(`timer pause ${name}`);
}
async function handleTimerResume(name: string) {
	await executeCommand(`timer resume ${name}`);
}
async function handleTimerStop(name: string) {
	await executeCommand(`timer stop ${name}`);
}

// --- Snippet actions ---

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
		// Fallback: use the backend command
		await executeCommand(`snip ${s.name}`);
		snippetCopied = s.id;
		setTimeout(() => {
			snippetCopied = null;
		}, 1500);
	}
}

const TAB_ORDER: TabId[] = ["notes", "todos", "reminders", "timers", "snippets"];

function handleKeydown(e: KeyboardEvent) {
	const inEditor = editingNote || isNewNote;
	if (e.key === "Escape" || (e.key === "Enter" && inEditor)) {
		e.preventDefault();
		if (inEditor) {
			backToList();
		} else {
			clearTimeout(saveTimer);
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
			<StickyNote size={11} strokeWidth={1.5} style="display:inline;vertical-align:-1px;margin-right:3px" />Notes{#if notes.length > 0}<span class="tab-badge" class:limit={notes.length >= MAX_NOTES}>{notes.length}/{MAX_NOTES}</span>{/if}
		</button>
		<button
			class="tab"
			class:active={activeTab === "todos"}
			onmousedown={(e) => e.preventDefault()}
			onclick={() => { activeTab = "todos"; }}
			tabindex={-1}
		>
			<ListChecks size={11} strokeWidth={1.5} style="display:inline;vertical-align:-1px;margin-right:3px" />Todos{#if todos.length > 0}<span class="tab-badge">{remainingTodos}/{todos.length}</span>{/if}
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
		<div class="section">
			{#if editingNote || isNewNote}
				<!-- Editor view -->
				<div class="note-editor-header">
					<button class="btn-back" onclick={backToList} onmousedown={(e) => e.preventDefault()} tabindex={-1}>
						<ChevronLeft size={14} strokeWidth={1.5} />
						<span>Back</span>
					</button>
					<span class="char-count" class:limit={editText.length >= MAX_NOTE_CHARS}>{editText.length}/{MAX_NOTE_CHARS}</span>
				</div>
				<textarea
					class="note-input"
					bind:value={editText}
					oninput={handleNoteInput}
					onblur={saveCurrentNote}
					placeholder="Write your note..."
					maxlength={MAX_NOTE_CHARS}
					rows={6}
				></textarea>
			{:else}
				<!-- List view -->
				{#if pendingNoteText}
					<div class="pending-banner">
						<div class="pending-header">
							<span class="pending-label">Pending — delete a note to make room</span>
							<button class="pending-cancel" onclick={cancelPending} onmousedown={(e) => e.preventDefault()} tabindex={-1} aria-label="Cancel">
								<X size={12} strokeWidth={1.5} />
							</button>
						</div>
						<div class="pending-text">{noteTitle(pendingNoteText)}</div>
					</div>
				{/if}
				{#if notes.length > 0}
					<ul class="note-list" role="list">
						{#each notes as note (note.id)}
							<li class="note-item" class:show-delete={!!pendingNoteText}>
								<button
									class="note-content"
									onclick={() => openNote(note)}
									onmousedown={(e) => e.preventDefault()}
									tabindex={-1}
								>
									<span class="note-title">{noteTitle(note.text)}</span>
								</button>
								<button
									class="note-delete"
									onclick={() => handleDeleteNote(note.id)}
									onmousedown={(e) => e.preventDefault()}
									tabindex={-1}
									aria-label="Delete"
								>
									<X size={12} strokeWidth={1.5} />
								</button>
							</li>
						{/each}
					</ul>
				{/if}
				{#if notes.length < MAX_NOTES}
					<button class="btn-add-note" onclick={handleNewNote} onmousedown={(e) => e.preventDefault()} tabindex={-1}>
						<Plus size={13} strokeWidth={1.5} />
						<span>New note</span>
					</button>
				{:else}
					<div class="note-limit-msg">{MAX_NOTES}/{MAX_NOTES} — delete a note to add more</div>
				{/if}
			{/if}
		</div>
	{:else if activeTab === "todos"}
		<div class="section">
			<div class="todo-add">
				<input
					class="todo-input"
					bind:value={todoInput}
					onkeydown={(e) => { if (e.key === "Enter") { e.preventDefault(); e.stopPropagation(); handleAddTodo(); } }}
					placeholder="Add todo..."
					type="text"
				/>
			</div>
			{#if sortedTodos.length > 0}
				<ul class="todo-list" role="list">
					{#each sortedTodos as todo (todo.id)}
						<li class="todo-item" class:done={todo.done}>
							<button
								class="todo-check"
								onclick={() => handleToggle(todo.id)}
								onmousedown={(e) => e.preventDefault()}
								tabindex={-1}
								aria-label={todo.done ? "Uncheck" : "Check"}
							>
								<span class="check-box" class:checked={todo.done}>
									{#if todo.done}&#10003;{/if}
								</span>
							</button>
							<span class="todo-text">{todo.text}</span>
							<button
								class="todo-delete"
								onclick={() => handleDeleteTodo(todo.id)}
								onmousedown={(e) => e.preventDefault()}
								tabindex={-1}
								aria-label="Delete"
							>
								<X size={12} strokeWidth={1.5} />
							</button>
						</li>
					{/each}
				</ul>
			{/if}
		</div>
	{:else if activeTab === "reminders"}
		<div class="section reminder-section">
			<div class="reminder-add">
				<input
					class="reminder-input"
					bind:value={reminderInput}
					onkeydown={(e) => { if (e.key === "Enter") { e.preventDefault(); e.stopPropagation(); handleAddReminder(); } }}
					placeholder="buy milk in 30m, standup at 9am..."
					type="text"
				/>
			</div>
			{#if activeReminders.length > 0 || firedReminders.length > 0}
				{#each activeReminders as rem (rem.id)}
					<div class="reminder-row">
						<div class="reminder-header">
							<span class="reminder-name">
								{rem.text}
							</span>
							<span class="reminder-countdown">
								{formatReminderTime(rem.due_at)}
							</span>
						</div>
						<div class="reminder-progress-bar">
							<div
								class="reminder-progress-fill"
								style="width: {reminderProgress(rem) * 100}%"
							></div>
						</div>
						<div class="reminder-meta">
							<span class="reminder-due">{formatReminderDueLabel(rem.due_at)}</span>
							<button
								class="reminder-ctrl stop"
								onclick={() => handleDeleteReminder(rem.id)}
								onmousedown={(e) => e.preventDefault()}
								tabindex={-1}
							>Dismiss</button>
						</div>
					</div>
				{/each}
				{#if firedReminders.length > 0}
					{#if activeReminders.length > 0}
						<div class="reminder-separator">
							<span class="reminder-separator-line"></span>
							<span class="reminder-separator-text">completed</span>
							<span class="reminder-separator-line"></span>
						</div>
					{/if}
					{#each firedReminders as rem (rem.id)}
						<div class="reminder-row fired">
							<div class="reminder-header">
								<span class="reminder-name">{rem.text}</span>
								<span class="reminder-fired-label">DONE</span>
							</div>
							<div class="reminder-meta">
								<span class="reminder-due">{formatReminderDueLabel(rem.due_at)}</span>
								<button
									class="reminder-ctrl stop"
									onclick={() => handleDeleteReminder(rem.id)}
									onmousedown={(e) => e.preventDefault()}
									tabindex={-1}
								>Remove</button>
							</div>
						</div>
					{/each}
				{/if}
			{:else}
				<div class="reminder-empty">No reminders. Add one above.</div>
			{/if}
		</div>
	{:else if activeTab === "timers"}
		<div class="section timer-section">
			<div class="timer-add">
				<input
					class="timer-input"
					bind:value={timerInput}
					onkeydown={(e) => { if (e.key === "Enter") { e.preventDefault(); e.stopPropagation(); handleCreateTimer(); } }}
					placeholder="25m, 1h30m, stopwatch..."
					type="text"
				/>
			</div>
			{#if timers.length === 0}
				<div class="timer-empty">No active timers. Add one above.</div>
			{:else}
				{#each timers as t (t.id)}
					<div class="timer-row" class:done={t.done}>
						<div class="timer-header">
							<span class="timer-name">
								{t.name}
								<span class="timer-badge" class:stopwatch={t.stopwatch}>
									{t.stopwatch ? "stopwatch" : "timer"}
								</span>
							</span>
							<span class="timer-remaining">
								{#if t.stopwatch}
									{#if t.paused}<span class="timer-paused-label">PAUSED</span>{/if}
									{formatTimerTime(t.elapsed_secs)}
								{:else if t.done}
									<span class="timer-done-label">DONE</span>
								{:else if t.paused}
									<span class="timer-paused-label">PAUSED</span>
									{formatTimerTime(t.remaining_secs)}
								{:else}
									{formatTimerTime(t.remaining_secs)}
								{/if}
							</span>
						</div>
						{#if !t.stopwatch}
							<div class="timer-progress-bar">
								<div
									class="timer-progress-fill"
									class:done={t.done}
									class:paused={t.paused}
									style="width: {timerProgress(t) * 100}%"
								></div>
							</div>
						{/if}
						<div class="timer-meta">
							<span class="timer-duration">
								{#if t.stopwatch}
									{formatTimerDuration(t.elapsed_secs)} elapsed
								{:else}
									{formatTimerDuration(t.duration_secs)}
								{/if}
							</span>
							<div class="timer-controls">
								{#if !t.done}
									{#if t.paused}
										<button class="timer-ctrl" onclick={() => handleTimerResume(t.name)} onmousedown={(e) => e.preventDefault()} tabindex={-1}>Resume</button>
									{:else}
										<button class="timer-ctrl" onclick={() => handleTimerPause(t.name)} onmousedown={(e) => e.preventDefault()} tabindex={-1}>Pause</button>
									{/if}
								{/if}
								<button class="timer-ctrl stop" onclick={() => handleTimerStop(t.name)} onmousedown={(e) => e.preventDefault()} tabindex={-1}>Stop</button>
							</div>
						</div>
					</div>
				{/each}
			{/if}
		</div>
	{:else if activeTab === "snippets"}
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

	.tab-badge.limit {
		color: var(--error);
	}

	.section {
		padding: 10px 20px;
	}

	/* --- Notes list --- */

	.note-list {
		list-style: none;
		padding: 0;
		margin: 0;
	}

	.note-item {
		display: flex;
		align-items: center;
		gap: 4px;
		border-bottom: 1px solid var(--border);
	}

	.note-item:last-child {
		border-bottom: none;
	}

	.note-content {
		flex: 1;
		display: flex;
		align-items: center;
		background: none;
		border: none;
		padding: 8px 0;
		cursor: pointer;
		text-align: left;
		min-width: 0;
	}

	.note-title {
		font-family: var(--font-mono);
		font-size: 13px;
		color: var(--fg);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.note-content:hover .note-title {
		color: var(--accent);
	}

	.note-delete {
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

	.note-item:hover .note-delete {
		opacity: 1;
	}

	.note-delete:hover {
		color: var(--error);
	}

	.note-item.show-delete .note-delete {
		opacity: 1;
	}

	/* --- Pending note banner --- */

	.pending-banner {
		background: var(--bg-secondary);
		border: 1px dashed var(--accent);
		border-radius: 6px;
		padding: 8px 10px;
		margin-bottom: 8px;
	}

	.pending-header {
		display: flex;
		align-items: center;
		justify-content: space-between;
		margin-bottom: 4px;
	}

	.pending-label {
		font-family: var(--font-mono);
		font-size: 10px;
		color: var(--fg-muted);
		text-transform: uppercase;
		letter-spacing: 0.3px;
	}

	.pending-cancel {
		display: flex;
		align-items: center;
		justify-content: center;
		background: none;
		border: none;
		color: var(--fg-muted);
		cursor: pointer;
		padding: 2px;
		border-radius: 3px;
		transition: color 100ms ease;
	}

	.pending-cancel:hover {
		color: var(--error);
	}

	.pending-text {
		font-family: var(--font-mono);
		font-size: 13px;
		color: var(--fg);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.btn-add-note {
		display: flex;
		align-items: center;
		gap: 4px;
		font-family: var(--font-mono);
		font-size: 12px;
		color: var(--fg-muted);
		background: none;
		border: 1px dashed var(--border);
		border-radius: 6px;
		padding: 6px 10px;
		cursor: pointer;
		margin-top: 6px;
		width: 100%;
		transition: color 100ms ease, border-color 100ms ease;
	}

	.btn-add-note:hover {
		color: var(--fg);
		border-color: var(--fg-muted);
	}

	.note-limit-msg {
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--fg-muted);
		text-align: center;
		padding: 8px 0;
		margin-top: 4px;
	}

	/* --- Note editor --- */

	.note-editor-header {
		display: flex;
		align-items: center;
		justify-content: space-between;
		margin-bottom: 6px;
	}

	.btn-back {
		display: flex;
		align-items: center;
		gap: 2px;
		font-family: var(--font-mono);
		font-size: 12px;
		color: var(--fg-muted);
		background: none;
		border: none;
		cursor: pointer;
		padding: 2px 0;
		transition: color 100ms ease;
	}

	.btn-back:hover {
		color: var(--fg);
	}

	.char-count {
		font-family: var(--font-mono);
		font-size: 10px;
		color: var(--fg-muted);
	}

	.char-count.limit {
		color: var(--error);
	}

	.note-input {
		width: 100%;
		background: var(--bg-secondary);
		border: 1px solid var(--border);
		border-radius: 6px;
		color: var(--fg);
		font-family: var(--font-mono);
		font-size: 13px;
		padding: 8px 10px;
		resize: none;
		outline: none;
		line-height: 1.5;
	}

	.note-input:focus {
		border-color: var(--fg-muted);
	}

	.note-input::placeholder {
		color: var(--fg-muted);
		opacity: 0.5;
	}

	/* --- Todos --- */

	.todo-add {
		margin-bottom: 4px;
	}

	.todo-input {
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

	.todo-input:focus {
		border-color: var(--fg-muted);
	}

	.todo-input::placeholder {
		color: var(--fg-muted);
		opacity: 0.5;
	}

	.todo-list {
		list-style: none;
		padding: 0;
		margin: 0;
	}

	.todo-item {
		display: flex;
		align-items: center;
		gap: 8px;
		padding: 4px 0;
		transition: opacity 100ms ease;
	}

	.todo-item.done {
		opacity: 0.5;
	}

	.todo-check {
		display: flex;
		align-items: center;
		justify-content: center;
		background: none;
		border: none;
		cursor: pointer;
		padding: 0;
		flex-shrink: 0;
	}

	.check-box {
		display: flex;
		align-items: center;
		justify-content: center;
		width: 14px;
		height: 14px;
		border: 1px solid var(--fg-muted);
		border-radius: 3px;
		font-size: 10px;
		color: var(--success);
		transition: border-color 100ms ease;
	}

	.check-box.checked {
		border-color: var(--success);
	}

	.todo-text {
		flex: 1;
		font-family: var(--font-mono);
		font-size: 13px;
		color: var(--fg);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.todo-item.done .todo-text {
		text-decoration: line-through;
		color: var(--fg-muted);
	}

	.todo-delete {
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

	.todo-item:hover .todo-delete {
		opacity: 1;
	}

	.todo-delete:hover {
		color: var(--error);
	}

	/* --- Reminders --- */

	.reminder-section {
		display: flex;
		flex-direction: column;
		gap: 10px;
	}

	.reminder-add {
		margin-bottom: 0;
	}

	.reminder-input {
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

	.reminder-input:focus {
		border-color: var(--fg-muted);
	}

	.reminder-input::placeholder {
		color: var(--fg-muted);
		opacity: 0.5;
	}

	.reminder-row {
		display: flex;
		flex-direction: column;
		gap: 5px;
	}

	.reminder-row + .reminder-row {
		padding-top: 10px;
		border-top: 1px solid var(--border);
	}

	.reminder-row.fired {
		opacity: 0.5;
	}

	.reminder-row.fired .reminder-name {
		text-decoration: line-through;
		color: var(--fg-muted);
	}

	.reminder-header {
		display: flex;
		justify-content: space-between;
		align-items: baseline;
	}

	.reminder-name {
		font-family: var(--font-mono);
		font-size: 13px;
		font-weight: 600;
		color: var(--fg);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		flex: 1;
		min-width: 0;
	}

	.reminder-countdown {
		font-family: var(--font-mono);
		font-size: 18px;
		font-weight: 600;
		color: var(--fg);
		font-variant-numeric: tabular-nums;
		flex-shrink: 0;
		margin-left: 8px;
	}

	.reminder-fired-label {
		font-family: var(--font-mono);
		font-size: 11px;
		font-weight: 600;
		color: var(--success, #4caf50);
		flex-shrink: 0;
	}

	.reminder-progress-bar {
		height: 3px;
		background: var(--bg);
		border-radius: 2px;
		overflow: hidden;
	}

	.reminder-progress-fill {
		height: 100%;
		background: var(--fg);
		border-radius: 2px;
		transition: width 200ms linear;
	}

	.reminder-meta {
		display: flex;
		justify-content: space-between;
		align-items: center;
	}

	.reminder-due {
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--fg-muted);
	}

	.reminder-ctrl {
		font-family: var(--font-mono);
		font-size: 10px;
		padding: 2px 8px;
		border-radius: 3px;
		border: 1px solid var(--border);
		background: transparent;
		color: var(--fg-muted);
		cursor: pointer;
		transition: color 100ms ease, background 100ms ease;
	}

	.reminder-ctrl:hover {
		color: var(--fg);
		background: var(--bg);
	}

	.reminder-ctrl.stop:hover {
		color: var(--error);
		border-color: var(--error);
	}

	.reminder-separator {
		display: flex;
		align-items: center;
		gap: 8px;
		padding: 2px 0;
	}

	.reminder-separator-line {
		flex: 1;
		height: 1px;
		background: var(--border);
	}

	.reminder-separator-text {
		font-family: var(--font-mono);
		font-size: 9px;
		text-transform: uppercase;
		letter-spacing: 0.06em;
		color: var(--fg-muted);
		opacity: 0.6;
	}

	.reminder-empty {
		font-family: var(--font-mono);
		font-size: 12px;
		color: var(--fg-muted);
		text-align: center;
		padding: 16px 0;
	}

	/* --- Timers --- */

	.timer-add {
		margin-bottom: 4px;
	}

	.timer-input {
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

	.timer-input:focus {
		border-color: var(--fg-muted);
	}

	.timer-input::placeholder {
		color: var(--fg-muted);
		opacity: 0.5;
	}

	.timer-section {
		display: flex;
		flex-direction: column;
		gap: 10px;
	}

	.timer-empty {
		font-family: var(--font-mono);
		font-size: 12px;
		color: var(--fg-muted);
		text-align: center;
		padding: 16px 0;
	}

	.timer-row {
		display: flex;
		flex-direction: column;
		gap: 5px;
	}

	.timer-row + .timer-row {
		padding-top: 10px;
		border-top: 1px solid var(--border);
	}

	.timer-header {
		display: flex;
		justify-content: space-between;
		align-items: baseline;
	}

	.timer-name {
		font-family: var(--font-mono);
		font-size: 13px;
		font-weight: 600;
		color: var(--fg);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		display: flex;
		align-items: center;
		gap: 6px;
	}

	.timer-badge {
		font-family: var(--font-mono);
		font-size: 9px;
		font-weight: 500;
		text-transform: uppercase;
		letter-spacing: 0.04em;
		padding: 1px 5px;
		border-radius: 3px;
		background: var(--bg);
		color: var(--fg-muted);
		border: 1px solid var(--border);
		flex-shrink: 0;
	}

	.timer-badge.stopwatch {
		color: var(--accent);
		border-color: var(--accent);
		opacity: 0.7;
	}

	.timer-remaining {
		font-family: var(--font-mono);
		font-size: 18px;
		font-weight: 600;
		color: var(--fg);
		font-variant-numeric: tabular-nums;
		flex-shrink: 0;
	}

	.timer-done-label {
		font-size: 11px;
		font-weight: 600;
		color: var(--success, #4caf50);
	}

	.timer-paused-label {
		font-size: 10px;
		font-weight: 500;
		color: var(--fg-muted);
		margin-right: 6px;
	}

	.timer-progress-bar {
		height: 3px;
		background: var(--bg);
		border-radius: 2px;
		overflow: hidden;
	}

	.timer-progress-fill {
		height: 100%;
		background: var(--fg);
		border-radius: 2px;
		transition: width 100ms linear;
	}

	.timer-progress-fill.paused {
		background: var(--fg-muted);
	}

	.timer-progress-fill.done {
		background: var(--success, #4caf50);
	}

	.timer-meta {
		display: flex;
		justify-content: space-between;
		align-items: center;
	}

	.timer-duration {
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--fg-muted);
	}

	.timer-controls {
		display: flex;
		gap: 6px;
	}

	.timer-ctrl {
		font-family: var(--font-mono);
		font-size: 10px;
		padding: 2px 8px;
		border-radius: 3px;
		border: 1px solid var(--border);
		background: transparent;
		color: var(--fg-muted);
		cursor: pointer;
		transition: color 100ms ease, background 100ms ease;
	}

	.timer-ctrl:hover {
		color: var(--fg);
		background: var(--bg);
	}

	.timer-ctrl.stop:hover {
		color: var(--error);
		border-color: var(--error);
	}

	/* --- Snippets --- */

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

<script lang="ts">
import { getCurrentWindow } from "@tauri-apps/api/window";
import { ChevronLeft, Plus, X } from "lucide-svelte";
import { onMount } from "svelte";
import type { NoteItem, TodoItem } from "$lib/ipc";
import {
	addNote,
	addTodo,
	deleteNote,
	deleteTodo,
	getNotes,
	getTodos,
	toggleTodo,
	updateNote,
} from "$lib/ipc";

const MAX_NOTES = 5;
const MAX_NOTE_CHARS = 500;

let {
	ondismiss,
	pendingNoteText = null,
	onpendingcleared,
}: {
	ondismiss: () => void;
	pendingNoteText?: string | null;
	onpendingcleared?: () => void;
} = $props();

let activeTab: "notes" | "todos" = $state("notes");

// Notes state
let notes: NoteItem[] = $state([]);
let editingNote: NoteItem | null = $state(null);
let isNewNote = $state(false);
let editText = $state("");
let saveTimer: ReturnType<typeof setTimeout> | undefined;

// Todos state
let todos: TodoItem[] = $state([]);
let todoInput = $state("");

// Derived
let sortedTodos = $derived.by(() => {
	const unchecked = todos.filter((t) => !t.done);
	const checked = todos.filter((t) => t.done);
	return [...unchecked, ...checked];
});
let remainingTodos = $derived(todos.filter((t) => !t.done).length);

onMount(() => {
	loadData();

	let unlisten: (() => void) | undefined;
	(async () => {
		const win = getCurrentWindow();
		unlisten = await win.listen("lychi://notes-changed", () => {
			clearTimeout(saveTimer);
			loadData();
		});
	})();

	return () => {
		unlisten?.();
	};
});

async function loadData() {
	clearTimeout(saveTimer);
	try {
		const [n, t] = await Promise.all([getNotes(), getTodos()]);
		notes = n;
		todos = t;
		// If editing a note that was deleted externally, go back to list
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
			Notes{#if notes.length > 0}<span class="tab-badge" class:limit={notes.length >= MAX_NOTES}>{notes.length}/{MAX_NOTES}</span>{/if}
		</button>
		<button
			class="tab"
			class:active={activeTab === "todos"}
			onmousedown={(e) => e.preventDefault()}
			onclick={() => { activeTab = "todos"; }}
			tabindex={-1}
		>
			Todos{#if todos.length > 0}<span class="tab-badge">{remainingTodos}/{todos.length}</span>{/if}
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
	{:else}
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
		padding: 8px 12px;
		cursor: pointer;
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
</style>

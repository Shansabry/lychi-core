<script lang="ts">
import { ChevronLeft, Plus, X } from "lucide-svelte";
import type { ScratchItem } from "$lib/ipc";
import { addNote, addTodo, deleteItem, toggleItem, updateNote } from "$lib/ipc";

const MAX_NOTE_CHARS = 500;

let {
	items = $bindable(),
	pendingNoteText = null,
	onpendingcleared,
}: {
	// Unified list: plain notes (done === null) and checklist lines (done !== null).
	items: ScratchItem[];
	pendingNoteText?: string | null;
	onpendingcleared?: () => void;
} = $props();

let editingNote: ScratchItem | null = $state(null);
let isNewNote = $state(false);
let editText = $state("");
let saveTimer: ReturnType<typeof setTimeout> | undefined;
let todoInput = $state("");

// Split the unified list for rendering: checklist lines grouped (unchecked
// first), plain notes shown as openable titles.
let checklist = $derived(
	items.filter((i) => i.done !== null).sort((a, b) => Number(a.done) - Number(b.done)),
);
let plainNotes = $derived(items.filter((i) => i.done === null));

export function isEditing(): boolean {
	return !!(editingNote || isNewNote);
}

export function backToList() {
	clearTimeout(saveTimer);
	if (editText.trim()) {
		saveCurrentNote();
	} else if (editingNote && !isNewNote) {
		handleDeleteNote(editingNote.id);
	}
	editingNote = null;
	isNewNote = false;
}

export function clearSaveTimer() {
	clearTimeout(saveTimer);
}

function noteTitle(text: string): string {
	const first = text.split("\n")[0] || text;
	return first.length > 50 ? `${first.slice(0, 50)}…` : first;
}

function openNote(note: ScratchItem) {
	editingNote = note;
	editText = note.text;
}

// --- Checklist (todo) actions ---

async function handleAddTodo() {
	const text = todoInput.trim();
	if (!text) return;
	try {
		const item = await addTodo(text);
		items = [
			{
				id: item.id,
				text: item.text,
				done: item.done,
				created_at: Date.now(),
				updated_at: Date.now(),
			},
			...items,
		];
		todoInput = "";
	} catch (err) {
		console.error("[notes] add checklist item error:", err);
	}
}

async function handleToggle(id: string) {
	try {
		await toggleItem(id);
		items = items.map((i) => (i.id === id && i.done !== null ? { ...i, done: !i.done } : i));
	} catch (err) {
		console.error("[notes] toggle error:", err);
	}
}

async function handleDeleteItem(id: string) {
	try {
		await deleteItem(id);
		items = items.filter((i) => i.id !== id);
	} catch (err) {
		console.error("[notes] delete error:", err);
	}
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
			const scratch: ScratchItem = {
				id: item.id,
				text: item.text,
				done: null,
				created_at: item.created_at,
				updated_at: item.updated_at,
			};
			items = [scratch, ...items];
			editingNote = scratch;
			isNewNote = false;
		} else if (editingNote) {
			await updateNote(editingNote.id, editText);
			items = items.map((n) =>
				n.id === editingNote?.id ? { ...n, text: editText, updated_at: Date.now() } : n,
			);
		}
	} catch (err) {
		console.error("[notes] save error:", err);
	}
}

async function handleDeleteNote(id: string) {
	try {
		await deleteItem(id);
		items = items.filter((n) => n.id !== id);
		if (editingNote?.id === id) editingNote = null;

		// A pending note was waiting for room (from the old cap); now unbounded,
		// but keep the auto-add-on-delete behavior for the sentinel path.
		if (pendingNoteText) {
			try {
				const item = await addNote(pendingNoteText);
				items = [
					{
						id: item.id,
						text: item.text,
						done: null,
						created_at: item.created_at,
						updated_at: item.updated_at,
					},
					...items,
				];
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
</script>

<div class="section">
	{#if editingNote || isNewNote}
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
			<!-- Checklist add + list -->
			<div class="todo-add">
				<input
					class="todo-input"
					bind:value={todoInput}
					onkeydown={(e) => { if (e.key === "Enter") { e.preventDefault(); e.stopPropagation(); handleAddTodo(); } }}
					placeholder="Add checklist item..."
					type="text"
				/>
			</div>
			{#if checklist.length > 0}
				<ul class="todo-list" role="list">
					{#each checklist as item (item.id)}
						<li class="todo-item" class:done={item.done}>
							<button
								class="todo-check"
								onclick={() => handleToggle(item.id)}
								onmousedown={(e) => e.preventDefault()}
								tabindex={-1}
								aria-label={item.done ? "Uncheck" : "Check"}
							>
								<span class="check-box" class:checked={item.done}>
									{#if item.done}&#10003;{/if}
								</span>
							</button>
							<span class="todo-text">{item.text}</span>
							<button
								class="todo-delete"
								onclick={() => handleDeleteItem(item.id)}
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

			<!-- Notes list -->
			{#if plainNotes.length > 0}
				<ul class="note-list" role="list">
					{#each plainNotes as note (note.id)}
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
			<button class="btn-add-note" onclick={handleNewNote} onmousedown={(e) => e.preventDefault()} tabindex={-1}>
				<Plus size={13} strokeWidth={1.5} />
				<span>New note</span>
			</button>
	{/if}
</div>

<style>
	.section {
		padding: 10px 20px;
	}

	.note-list {
		list-style: none;
		padding: 0;
		margin: 0;
	}

	/* --- Checklist (unified todo lines) --- */
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
		margin: 0 0 8px 0;
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
</style>

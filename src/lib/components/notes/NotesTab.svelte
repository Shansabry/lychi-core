<script lang="ts">
import { ChevronLeft, Plus, X } from "lucide-svelte";
import type { NoteItem } from "$lib/ipc";
import { addNote, deleteNote, updateNote } from "$lib/ipc";

const MAX_NOTES = 5;
const MAX_NOTE_CHARS = 500;

let {
	notes = $bindable(),
	pendingNoteText = null,
	onpendingcleared,
}: {
	notes: NoteItem[];
	pendingNoteText?: string | null;
	onpendingcleared?: () => void;
} = $props();

let editingNote: NoteItem | null = $state(null);
let isNewNote = $state(false);
let editText = $state("");
let saveTimer: ReturnType<typeof setTimeout> | undefined;

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

async function handleDeleteNote(id: string) {
	try {
		await deleteNote(id);
		notes = notes.filter((n) => n.id !== id);
		if (editingNote?.id === id) editingNote = null;

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

<style>
	.section {
		padding: 10px 20px;
	}

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

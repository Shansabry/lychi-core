<script lang="ts">
import { X } from "lucide-svelte";
import { onMount } from "svelte";
import type { TodoItem } from "$lib/ipc";
import { addTodo, deleteTodo, getNote, getTodos, setNote, toggleTodo } from "$lib/ipc";

let {
	ondismiss,
}: {
	ondismiss: () => void;
} = $props();

const MAX_NOTE_CHARS = 500;

let activeTab: "note" | "todos" = $state("note");
let note = $state("");
let todos: TodoItem[] = $state([]);
let todoInput = $state("");
let saveTimer: ReturnType<typeof setTimeout> | undefined;

// Sort: unchecked first, then checked
let sortedTodos = $derived.by(() => {
	const unchecked = todos.filter((t) => !t.done);
	const checked = todos.filter((t) => t.done);
	return [...unchecked, ...checked];
});

let charCount = $derived(note.length);
let remainingTodos = $derived(todos.filter((t) => !t.done).length);

onMount(() => {
	loadData();
});

async function loadData() {
	try {
		const [n, t] = await Promise.all([getNote(), getTodos()]);
		note = n;
		todos = t;
	} catch (err) {
		console.error("[notes] load error:", err);
	}
}

function handleNoteInput() {
	if (note.length > MAX_NOTE_CHARS) {
		note = note.slice(0, MAX_NOTE_CHARS);
	}
	clearTimeout(saveTimer);
	saveTimer = setTimeout(saveNote, 500);
}

async function saveNote() {
	try {
		await setNote(note);
	} catch (err) {
		console.error("[notes] save error:", err);
	}
}

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

async function handleDelete(id: string) {
	try {
		await deleteTodo(id);
		todos = todos.filter((t) => t.id !== id);
	} catch (err) {
		console.error("[notes] delete error:", err);
	}
}

function handleKeydown(e: KeyboardEvent) {
	if (e.key === "Escape") {
		e.preventDefault();
		// Save note before dismissing
		clearTimeout(saveTimer);
		saveNote();
		ondismiss();
	}
}
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="notes-panel" onkeydown={handleKeydown}>
	<div class="tab-bar">
		<button
			class="tab"
			class:active={activeTab === "note"}
			onmousedown={(e) => e.preventDefault()}
			onclick={() => { activeTab = "note"; }}
			tabindex={-1}
		>
			Note{#if charCount > 0}<span class="tab-badge" class:limit={charCount >= MAX_NOTE_CHARS}>{charCount}/{MAX_NOTE_CHARS}</span>{/if}
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

	{#if activeTab === "note"}
		<div class="section">
			<textarea
				class="note-input"
				bind:value={note}
				oninput={handleNoteInput}
				onblur={saveNote}
				placeholder="Quick note..."
				maxlength={MAX_NOTE_CHARS}
				rows={5}
			></textarea>
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
								onclick={() => handleDelete(todo.id)}
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

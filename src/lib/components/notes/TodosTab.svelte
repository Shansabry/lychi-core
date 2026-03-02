<script lang="ts">
import { X } from "lucide-svelte";
import type { TodoItem } from "$lib/ipc";
import { addTodo, deleteTodo, toggleTodo } from "$lib/ipc";

let {
	todos = $bindable(),
}: {
	todos: TodoItem[];
} = $props();

let todoInput = $state("");

let sortedTodos = $derived.by(() => {
	const unchecked = todos.filter((t) => !t.done);
	const checked = todos.filter((t) => t.done);
	return [...unchecked, ...checked];
});

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
</script>

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

<style>
	.section {
		padding: 10px 20px;
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

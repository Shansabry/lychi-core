<script lang="ts">
import { MessageSquare, Trash2 } from "lucide-svelte";
import type { ConversationSummary } from "$lib/ipc";

let {
	conversations,
	visible = false,
	onselect,
	ondelete,
	onclear,
	ondismiss,
}: {
	conversations: ConversationSummary[];
	/** Whether the panel is the active surface (gates keyboard nav). */
	visible?: boolean;
	/** Open a conversation to continue it. */
	onselect: (id: string) => void;
	/** Delete one conversation. */
	ondelete: (id: string) => void;
	/** Clear all history. */
	onclear: () => void;
	ondismiss: () => void;
} = $props();

let filterQuery = $state("");
let selectedIndex = $state(0);
let filterEl: HTMLInputElement | undefined = $state();

let displayed = $derived.by(() => {
	const q = filterQuery.trim().toLowerCase();
	if (!q) return conversations;
	return conversations.filter((c) => c.title.toLowerCase().includes(q));
});

// Clamp the selection whenever the list changes; focus the filter box on show.
$effect(() => {
	if (selectedIndex >= displayed.length) selectedIndex = Math.max(0, displayed.length - 1);
});
$effect(() => {
	if (visible) {
		selectedIndex = 0;
		requestAnimationFrame(() => filterEl?.focus());
	}
});

// Keyboard navigation while the panel is the active surface. Arrows move the
// selection, Enter opens it, ⌘/Ctrl+Backspace deletes it. Typing still filters
// (the filter input keeps focus). Escape is left to the global dismiss handler.
function onKeydown(e: KeyboardEvent) {
	if (!visible) return;
	if (e.key === "ArrowDown") {
		e.preventDefault();
		selectedIndex = Math.min(displayed.length - 1, selectedIndex + 1);
		scrollSelectedIntoView();
	} else if (e.key === "ArrowUp") {
		e.preventDefault();
		selectedIndex = Math.max(0, selectedIndex - 1);
		scrollSelectedIntoView();
	} else if (e.key === "Enter") {
		const sel = displayed[selectedIndex];
		if (sel) {
			e.preventDefault();
			e.stopPropagation();
			onselect(sel.id);
		}
	} else if ((e.metaKey || e.ctrlKey) && (e.key === "Backspace" || e.key === "Delete")) {
		const sel = displayed[selectedIndex];
		if (sel) {
			e.preventDefault();
			ondelete(sel.id);
		}
	}
}

let listEl: HTMLUListElement | undefined = $state();
function scrollSelectedIntoView() {
	requestAnimationFrame(() => {
		listEl?.querySelectorAll<HTMLElement>(".ch-item")[selectedIndex]?.scrollIntoView({
			block: "nearest",
		});
	});
}

/** Relative time like "2m", "3h", "5d" from an epoch-ms timestamp. */
function ago(ms: number): string {
	const s = Math.max(0, Math.floor((Date.now() - ms) / 1000));
	if (s < 60) return "now";
	const m = Math.floor(s / 60);
	if (m < 60) return `${m}m`;
	const h = Math.floor(m / 60);
	if (h < 24) return `${h}h`;
	const d = Math.floor(h / 24);
	return `${d}d`;
}
</script>

<svelte:window onkeydown={onKeydown} />
<div class="chat-history">
	<div class="ch-header">
		<div class="ch-filter">
			<MessageSquare size={13} strokeWidth={2} />
			<input
				type="text"
				placeholder="Recall a conversation…"
				bind:value={filterQuery}
				bind:this={filterEl}
				spellcheck="false"
				autocomplete="off"
			/>
		</div>
		{#if conversations.length > 0}
			<button class="ch-clear" onclick={onclear} onmousedown={(e) => e.preventDefault()} tabindex={-1}>
				Clear all
			</button>
		{/if}
	</div>

	{#if displayed.length > 0}
		<ul class="ch-list" role="listbox" bind:this={listEl}>
			{#each displayed as c, i (c.id)}
				<li class="ch-item" class:selected={i === selectedIndex}>
					<button
						class="ch-content"
						onclick={() => onselect(c.id)}
						onmousemove={() => (selectedIndex = i)}
						onmousedown={(e) => e.preventDefault()}
						tabindex={-1}
					>
						<span class="ch-title-row">
							{#if c.preset_label}
								<span class="ch-pill">{c.preset_label}</span>
							{/if}
							<span class="ch-title">{c.title}</span>
						</span>
						<span class="ch-meta">{c.turn_count} turn{c.turn_count === 1 ? "" : "s"} · {ago(c.updated_at)}</span>
					</button>
					<button
						class="ch-delete"
						onclick={() => ondelete(c.id)}
						onmousedown={(e) => e.preventDefault()}
						tabindex={-1}
						aria-label="Delete conversation"
					>
						<Trash2 size={13} strokeWidth={2} />
					</button>
				</li>
			{/each}
		</ul>
	{:else if filterQuery}
		<div class="ch-empty">No conversations match "{filterQuery}"</div>
	{:else}
		<div class="ch-empty">No past conversations yet. Ask the AI something, then find it here.</div>
	{/if}
</div>

<style>
	.chat-history {
		display: flex;
		flex-direction: column;
		max-height: 340px;
	}
	.ch-header {
		display: flex;
		align-items: center;
		gap: 8px;
		padding: 10px 16px 8px;
		border-bottom: 1px solid var(--border);
	}
	.ch-filter {
		flex: 1;
		display: flex;
		align-items: center;
		gap: 7px;
		color: var(--fg-muted);
	}
	.ch-filter input {
		flex: 1;
		background: none;
		border: none;
		outline: none;
		color: var(--fg);
		font-family: var(--font-mono);
		font-size: 13px;
	}
	.ch-filter input::placeholder {
		color: var(--fg-muted);
		opacity: 0.6;
	}
	.ch-clear {
		font-family: var(--font-mono);
		font-size: 10px;
		color: var(--fg-muted);
		background: none;
		border: none;
		cursor: pointer;
		padding: 2px 6px;
	}
	.ch-clear:hover {
		color: var(--error);
	}
	.ch-list {
		list-style: none;
		margin: 0;
		padding: 4px 0;
		overflow-y: auto;
	}
	.ch-item {
		display: flex;
		align-items: center;
		gap: 4px;
		padding: 0 10px 0 16px;
	}
	.ch-item:hover,
	.ch-item.selected {
		background: color-mix(in srgb, var(--fg) 5%, transparent);
	}
	.ch-item.selected {
		box-shadow: inset 2px 0 0 var(--accent);
	}
	.ch-content {
		flex: 1;
		display: flex;
		flex-direction: column;
		gap: 2px;
		background: none;
		border: none;
		padding: 8px 0;
		cursor: pointer;
		text-align: left;
		min-width: 0;
	}
	.ch-title-row {
		display: flex;
		align-items: center;
		gap: 7px;
		min-width: 0;
	}
	.ch-pill {
		flex-shrink: 0;
		font-family: var(--font-mono);
		font-size: 9.5px;
		line-height: 1;
		text-transform: lowercase;
		letter-spacing: 0.02em;
		padding: 2px 6px;
		border-radius: 4px;
		color: var(--accent);
		background: color-mix(in srgb, var(--accent) 12%, transparent);
		border: 1px solid color-mix(in srgb, var(--accent) 30%, transparent);
	}
	.ch-title {
		font-family: var(--font-sans, system-ui);
		font-size: 13px;
		color: var(--fg);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		min-width: 0;
	}
	.ch-content:hover .ch-title {
		color: var(--accent);
	}
	.ch-meta {
		font-family: var(--font-mono);
		font-size: 10.5px;
		color: var(--fg-muted);
	}
	.ch-delete {
		display: flex;
		align-items: center;
		justify-content: center;
		background: none;
		border: none;
		color: var(--fg-muted);
		cursor: pointer;
		padding: 4px;
		border-radius: 4px;
		opacity: 0;
		flex-shrink: 0;
	}
	.ch-item:hover .ch-delete {
		opacity: 1;
	}
	.ch-delete:hover {
		color: var(--error);
	}
	.ch-empty {
		font-family: var(--font-mono);
		font-size: 12px;
		color: var(--fg-muted);
		text-align: center;
		padding: 24px 16px;
		line-height: 1.5;
	}
</style>

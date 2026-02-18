<script lang="ts">
import { fuzzyRank } from "$lib/fuzzy";

let {
	entries,
	onselect,
}: {
	entries: string[];
	onselect: (entry: string) => void;
} = $props();

let filterQuery = $state("");

let displayed = $derived.by(() => {
	if (!filterQuery.trim()) {
		// Deduplicate, keeping most recent (reverse order)
		const seen = new Set<string>();
		const result: string[] = [];
		for (let i = entries.length - 1; i >= 0; i--) {
			if (!seen.has(entries[i])) {
				seen.add(entries[i]);
				result.push(entries[i]);
			}
		}
		return result;
	}
	return fuzzyRank(filterQuery.trim(), entries).map((m) => m.value);
});
</script>

<div class="history-filter">
	<input
		type="text"
		placeholder="filter history..."
		bind:value={filterQuery}
		spellcheck="false"
		autocomplete="off"
	/>
</div>
{#if displayed.length > 0}
	<ul class="history" role="listbox">
		{#each displayed as entry}
			<li
				class="history-item"
				onmousedown={(e) => e.preventDefault()}
				onclick={() => onselect(entry)}
				onkeydown={(e) => e.key === "Enter" && onselect(entry)}
				role="option"
				aria-selected={false}
				tabindex={-1}
			>
				<span class="entry-icon">↩</span>
				<span class="entry-text">{entry}</span>
			</li>
		{/each}
	</ul>
{:else if filterQuery}
	<div class="history-empty">No matches for "{filterQuery}"</div>
{:else}
	<div class="history-empty">No history yet</div>
{/if}

<style>
	.history-filter {
		padding: 6px 20px;
		border-bottom: 1px solid var(--border);
	}

	.history-filter input {
		width: 100%;
		background: none;
		border: none;
		outline: none;
		font-family: var(--font-mono);
		font-size: 12px;
		color: var(--fg);
	}

	.history-filter input::placeholder {
		color: var(--fg-muted);
		opacity: 0.5;
	}

	.history {
		list-style: none;
		overflow-y: auto;
		max-height: 280px;
	}

	.history-item {
		display: flex;
		align-items: center;
		gap: 10px;
		padding: 6px 20px;
		cursor: pointer;
		transition: background 60ms ease;
	}

	.history-item:hover {
		background: var(--bg-secondary);
	}

	.entry-icon {
		color: var(--fg-muted);
		font-size: 12px;
		flex-shrink: 0;
	}

	.entry-text {
		font-family: var(--font-mono);
		font-size: 13px;
		color: var(--fg);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.history-empty {
		padding: 12px 20px;
		font-family: var(--font-mono);
		font-size: 12px;
		color: var(--fg-muted);
	}
</style>

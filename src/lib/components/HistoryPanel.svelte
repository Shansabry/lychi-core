<script lang="ts">
let {
	entries,
	onselect,
}: {
	entries: string[];
	onselect: (entry: string) => void;
} = $props();

// Show most recent first
let reversed = $derived([...entries].reverse());
</script>

{#if reversed.length > 0}
	<ul class="history" role="listbox">
		{#each reversed as entry}
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
{:else}
	<div class="history-empty">No history yet</div>
{/if}

<style>
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

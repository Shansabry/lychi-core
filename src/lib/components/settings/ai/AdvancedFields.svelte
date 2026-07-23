<script lang="ts">
import type { Snippet } from "svelte";

let {
	open = $bindable(false),
	children,
}: {
	open?: boolean;
	children: Snippet;
} = $props();
</script>

<div class="disclosure" class:open>
	<button
		class="head"
		type="button"
		onclick={() => (open = !open)}
		aria-expanded={open}
	>
		<span class="tri">▶</span> Advanced
		<span class="kbd">⌥A</span>
	</button>
	<div class="body">
		{@render children()}
	</div>
</div>

<style>
	.disclosure {
		margin-top: 20px;
		border-top: 1px solid color-mix(in srgb, var(--border) 60%, transparent);
		padding-top: 14px;
	}
	.head {
		display: flex;
		align-items: center;
		gap: 8px;
		background: none;
		border: none;
		color: var(--fg-muted);
		font-family: var(--font-mono);
		font-size: 12px;
		cursor: pointer;
		padding: 0;
	}
	.head:hover {
		color: var(--fg);
	}
	.tri {
		transition: transform 0.15s;
		font-size: 9px;
		color: var(--fg-muted);
	}
	.disclosure.open .tri {
		transform: rotate(90deg);
	}
	.kbd {
		font-family: var(--font-mono);
		font-size: 10px;
		color: var(--fg-muted);
		background: var(--bg-secondary);
		border: 1px solid var(--border);
		border-radius: 4px;
		padding: 0 4px;
		margin-left: 2px;
	}
	.body {
		display: none;
		padding-top: 6px;
	}
	.disclosure.open .body {
		display: block;
	}
</style>

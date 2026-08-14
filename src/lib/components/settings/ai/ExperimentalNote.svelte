<script lang="ts">
// A small "Experimental" advisory banner shared by the local-AI provider
// panels (built-in local models and Ollama). The body text differs per
// provider — the styling lives here once so the two can't drift apart.
let { children }: { children: import("svelte").Snippet } = $props();
</script>

<div class="experimental" role="note">
	<span class="experimental-tag">Experimental</span>
	<p class="experimental-body">{@render children()}</p>
</div>

<style>
	.experimental {
		display: flex;
		gap: 11px;
		align-items: flex-start;
		padding: 12px 14px;
		border: 1px solid color-mix(in srgb, var(--warning) 35%, transparent);
		background: color-mix(in srgb, var(--warning) 7%, var(--bg-secondary));
		border-radius: 9px;
		margin-top: 4px;
	}
	.experimental-tag {
		flex-shrink: 0;
		font-size: 9.5px;
		font-weight: 600;
		letter-spacing: 0.06em;
		text-transform: uppercase;
		color: var(--warning);
		border: 1px solid color-mix(in srgb, var(--warning) 45%, transparent);
		border-radius: 5px;
		padding: 2px 7px;
		margin-top: 1px;
	}
	.experimental-body {
		margin: 0;
		font-size: 11.5px;
		line-height: 1.55;
		color: var(--fg-muted);
	}
</style>

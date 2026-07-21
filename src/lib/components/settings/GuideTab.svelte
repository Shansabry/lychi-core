<script lang="ts">
import { onMount } from "svelte";
import { type CommandInfo, getCommandCatalog, getTriggerCatalog } from "$lib/ipc";

let guideTab: "commands" | "triggers" = $state("commands");

// Both lists are generated from the LIVE action registry, so they never go
// stale — registering a handler makes it appear automatically, and a trigger's
// description comes from the same handler as its command (centralised).
let commands: CommandInfo[] = $state([]);
let triggers: CommandInfo[] = $state([]);

onMount(async () => {
	try {
		[commands, triggers] = await Promise.all([getCommandCatalog(), getTriggerCatalog()]);
	} catch {
		commands = [];
		triggers = [];
	}
});
</script>

<div class="guide" role="region" aria-label="Guide">
	<div class="guide-tab-bar">
		<button
			class="guide-tab"
			class:active={guideTab === "commands"}
			onmousedown={(e) => e.preventDefault()}
			onclick={() => { guideTab = "commands"; }}
			tabindex={-1}
		>Commands</button>
		<button
			class="guide-tab"
			class:active={guideTab === "triggers"}
			onmousedown={(e) => e.preventDefault()}
			onclick={() => { guideTab = "triggers"; }}
			tabindex={-1}
		>Triggers</button>
	</div>

	{#if guideTab === "commands"}
		<div class="guide-table">
			{#each commands as cmd (cmd.id)}
				<div class="guide-row">
					<code>{cmd.keyword}</code>
					<span>{cmd.description}</span>
				</div>
			{:else}
				<div class="guide-empty">Loading commands…</div>
			{/each}
		</div>
	{:else}
		<div class="guide-table">
			{#each triggers as trig (trig.keyword)}
				<div class="guide-row">
					<code>{trig.keyword}</code>
					<span>{trig.description}</span>
				</div>
			{:else}
				<div class="guide-empty">Loading triggers…</div>
			{/each}
		</div>
	{/if}
</div>

<style>
	.guide {
		display: flex;
		flex-direction: column;
		padding: 2px 0;
		/* No inner scroll — the SettingsPanel `.content` wrapper already scrolls.
		   A second overflow here produced a double scrollbar (only on this tab). */
	}

	.guide-tab-bar {
		display: flex;
		border-bottom: 1px solid var(--border);
		margin-bottom: 8px;
		/* Keep the Commands/Triggers switch visible while the list scrolls under
		   the panel's single scrollbar. */
		position: sticky;
		top: 0;
		background: var(--bg-secondary);
		z-index: 1;
	}

	.guide-tab {
		font-family: var(--font-mono);
		font-size: 11px;
		text-transform: uppercase;
		letter-spacing: 0.5px;
		color: var(--fg-muted);
		background: none;
		border: none;
		border-bottom: 2px solid transparent;
		padding: 6px 10px;
		cursor: pointer;
		transition: color 100ms ease, border-color 100ms ease;
	}

	.guide-tab:hover {
		color: var(--fg);
	}

	.guide-tab.active {
		color: var(--fg);
		border-bottom-color: var(--accent);
	}

	.guide-table {
		display: flex;
		flex-direction: column;
		gap: 1px;
	}

	.guide-row {
		display: flex;
		align-items: center;
		gap: 12px;
		padding: 3px 0;
	}

	.guide-row code {
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--fg);
		background: var(--bg-secondary);
		padding: 2px 6px;
		border-radius: 3px;
		border: 1px solid var(--border);
		white-space: nowrap;
		min-width: 100px;
		text-align: center;
	}

	.guide-row span {
		font-size: 11px;
		color: var(--fg-muted);
	}

	.guide-empty {
		font-size: 11px;
		color: var(--fg-muted);
		padding: 8px 0;
	}
</style>

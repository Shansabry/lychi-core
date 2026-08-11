<script lang="ts">
import type { Snippet } from "svelte";
import type { AiConfig } from "$lib/ipc";

let {
	open = $bindable(false),
	aiConfig = $bindable(),
	onsave,
	children,
}: {
	open?: boolean;
	/** The global AI config — Timeout + Max Tokens live on it and apply to
	 *  every mode, so they belong here, not duplicated in each mode's fields. */
	aiConfig: AiConfig;
	/** Persist the current config (called on every field edit). */
	onsave: () => void;
	/** Optional mode-specific advanced fields rendered above the shared ones. */
	children?: Snippet;
} = $props();

// Below this, a normal agent answer (a few paragraphs of markdown, or a tool
// call plus explanation) routinely hits the cap and stops mid-sentence. Warn
// so a low value is a deliberate choice, not a silent truncation.
const LOW_MAX_TOKENS = 512;
let maxTokensLow = $derived(
	typeof aiConfig.max_tokens === "number" && aiConfig.max_tokens < LOW_MAX_TOKENS,
);
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
		{#if children}{@render children()}{/if}

		<div class="field">
			<label for="ai-timeout">Timeout</label>
			<div class="number-row">
				<input
					id="ai-timeout"
					class="control num"
					type="number"
					min="2"
					max="120"
					bind:value={aiConfig.timeout_secs}
					onchange={onsave}
				/>
				<span class="unit">s</span>
			</div>
		</div>

		<div class="field">
			<label for="ai-max-tokens">Max Tokens</label>
			<input
				id="ai-max-tokens"
				class="control num"
				class:warn={maxTokensLow}
				type="number"
				min="256"
				max="16000"
				bind:value={aiConfig.max_tokens}
				onchange={onsave}
			/>
		</div>
		{#if maxTokensLow}
			<div class="hint warn">
				Low limit — answers may be cut off mid-sentence. 1024+ is recommended
				for the agent.
			</div>
		{/if}
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

	.field {
		display: grid;
		grid-template-columns: 120px 1fr;
		align-items: center;
		gap: 14px;
		padding: 9px 0;
	}
	.field label {
		font-size: 13px;
		color: var(--fg-muted);
	}
	.number-row {
		display: flex;
		align-items: center;
		gap: 8px;
	}
	.unit {
		font-size: 12px;
		color: var(--fg-muted);
	}
	.control.num {
		width: 90px;
	}
	.control.num.warn {
		border-color: var(--warning-muted);
	}
	.hint {
		font-size: 11px;
		color: var(--fg-muted);
		padding: 2px 0 6px 134px;
		line-height: 1.4;
	}
	.hint.warn {
		color: var(--warning-muted);
	}
</style>

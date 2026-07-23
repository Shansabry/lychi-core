<script lang="ts">
import type { AiConfig, OllamaModelInfo } from "$lib/ipc";
import { listOllamaModels } from "$lib/ipc";
import Select from "../../../Select.svelte";
import AdvancedFields from "../AdvancedFields.svelte";

let {
	aiConfig = $bindable(),
	advancedOpen = $bindable(false),
	onsave,
}: {
	aiConfig: AiConfig;
	advancedOpen?: boolean;
	onsave: () => void;
} = $props();

let ollamaModels: OllamaModelInfo[] = $state([]);
let ollamaFetchError: string | null = $state(null);

export async function fetchOllamaModels() {
	ollamaFetchError = null;
	try {
		ollamaModels = await listOllamaModels();
		if (ollamaModels.length > 0 && !aiConfig.ollama_model) {
			aiConfig.ollama_model = ollamaModels[0].name;
			onsave();
		}
	} catch {
		ollamaModels = [];
		ollamaFetchError = `Cannot connect to Ollama at ${aiConfig.ollama_url}`;
	}
}

function formatSize(bytes: number): string {
	if (bytes >= 1e9) return `${(bytes / 1e9).toFixed(1)} GB`;
	if (bytes >= 1e6) return `${(bytes / 1e6).toFixed(0)} MB`;
	return `${bytes} B`;
}

function handleOllamaModelChange(val: string) {
	aiConfig.ollama_model = val;
	onsave();
}

function handleOllamaUrlChange() {
	onsave();
	fetchOllamaModels();
}

// Fetch on mount / whenever the URL changes externally.
$effect(() => {
	void aiConfig.ollama_url;
	fetchOllamaModels();
});
</script>

<div class="field">
	<label for="ollama-url">URL</label>
	<input
		id="ollama-url"
		class="control"
		type="text"
		bind:value={aiConfig.ollama_url}
		placeholder="http://localhost:11434"
		spellcheck="false"
		onchange={handleOllamaUrlChange}
	/>
</div>

<div class="field">
	<label for="ollama-model">Model</label>
	{#if ollamaModels.length > 0}
		<Select
			id="ollama-model"
			value={aiConfig.ollama_model ?? ""}
			options={ollamaModels.map((m) => ({
				value: m.name,
				label: `${m.name} — ${formatSize(m.size)}`,
			}))}
			onchange={handleOllamaModelChange}
		/>
	{:else if ollamaFetchError}
		<span class="hint error">{ollamaFetchError}</span>
	{:else}
		<span class="hint">No models found — run <code>ollama pull &lt;model&gt;</code></span>
	{/if}
</div>

<AdvancedFields bind:open={advancedOpen}>
	<div class="field">
		<label for="ollama-timeout">Timeout</label>
		<div class="number-row">
			<input
				id="ollama-timeout"
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
		<label for="ollama-max-tokens">Max Tokens</label>
		<input
			id="ollama-max-tokens"
			class="control num"
			type="number"
			min="256"
			max="16000"
			bind:value={aiConfig.max_tokens}
			onchange={onsave}
		/>
	</div>
</AdvancedFields>

<style>
	.field {
		display: grid;
		grid-template-columns: 120px 1fr;
		align-items: center;
		gap: 14px;
		padding: 9px 0;
		border-top: 1px solid color-mix(in srgb, var(--border) 60%, transparent);
	}
	.field:first-child {
		border-top: none;
	}
	label {
		font-size: 12.5px;
		color: var(--fg-muted);
	}
	.control {
		font-family: var(--font-mono);
		font-size: 12.5px;
		color: var(--fg);
		background: var(--bg-secondary);
		border: 1px solid var(--border);
		border-radius: 7px;
		padding: 8px 11px;
		width: 100%;
		outline: none;
	}
	.control:focus {
		border-color: var(--fg-muted);
	}
	.number-row {
		display: flex;
		align-items: center;
		gap: 6px;
	}
	.control.num {
		width: 90px;
		text-align: right;
	}
	.unit {
		font-size: 12px;
		color: var(--fg-muted);
	}
	.hint {
		font-size: 11px;
		color: var(--fg-muted);
	}
	.hint.error {
		color: var(--error);
	}
	.hint code {
		font-family: var(--font-mono);
		background: var(--bg-secondary);
		padding: 1px 4px;
		border-radius: 3px;
	}
</style>

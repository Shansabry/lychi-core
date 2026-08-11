<script lang="ts">
import type { AiConfig } from "$lib/ipc";
import Select from "../../../Select.svelte";
import AdvancedFields from "../AdvancedFields.svelte";
import ApiKeyField from "../ApiKeyField.svelte";

let {
	aiConfig = $bindable(),
	advancedOpen = $bindable(false),
	onsave,
	onhealthchange,
	apiKeyRef = $bindable(),
}: {
	aiConfig: AiConfig;
	advancedOpen?: boolean;
	/** Persist the current config (called on every field edit). */
	onsave: () => void;
	/** Re-check health (after an API key is set/cleared). */
	onhealthchange: () => void;
	apiKeyRef?: ApiKeyField;
} = $props();

type Preset = {
	id: string;
	label: string;
	base_url: string;
	wire_format: string;
	model_hint?: string;
	editable_url?: boolean;
};

const BYO_PRESETS: Preset[] = [
	{
		id: "anthropic",
		label: "Anthropic",
		base_url: "https://api.anthropic.com/v1/messages",
		wire_format: "anthropic",
		model_hint: "e.g. claude-sonnet-4-5-20250929, claude-haiku-4-5-20251001",
	},
	{
		id: "openai",
		label: "OpenAI",
		base_url: "https://api.openai.com/v1/chat/completions",
		wire_format: "openai",
		model_hint: "e.g. gpt-4o-mini, gpt-4o, gpt-4.1-mini",
	},
	{
		id: "groq",
		label: "Groq",
		base_url: "https://api.groq.com/openai/v1/chat/completions",
		wire_format: "openai",
		model_hint: "e.g. llama-3.3-70b-versatile, llama-3.1-8b-instant",
	},
	{
		id: "grok",
		label: "Grok (xAI)",
		base_url: "https://api.x.ai/v1/chat/completions",
		wire_format: "openai",
		model_hint: "e.g. grok-2-latest, grok-beta",
	},
	{
		id: "gemini",
		label: "Gemini (Google)",
		base_url: "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions",
		wire_format: "openai",
		model_hint: "e.g. gemini-2.0-flash, gemini-1.5-pro",
	},
	{
		id: "openrouter",
		label: "OpenRouter",
		base_url: "https://openrouter.ai/api/v1/chat/completions",
		wire_format: "openai",
		model_hint: "any OpenRouter model, e.g. anthropic/claude-3.5-sonnet",
		editable_url: true,
	},
	{
		id: "custom",
		label: "Custom (OpenAI-compatible)",
		base_url: "",
		wire_format: "openai",
		model_hint: "whatever your endpoint accepts",
		editable_url: true,
	},
];

const currentPreset = $derived(
	BYO_PRESETS.find((p) => p.id === aiConfig.provider) ?? BYO_PRESETS[0],
);
const showBaseUrl = $derived(
	currentPreset.editable_url === true || (aiConfig.base_url ?? "").trim() !== "",
);

function handleProviderChange(val: string) {
	aiConfig.provider = val;
	const preset = BYO_PRESETS.find((p) => p.id === val) ?? BYO_PRESETS[0];
	// Adopt the preset endpoint + wire format; don't carry a stale override from
	// another provider. Model is user-typed, so clear it.
	aiConfig.base_url = preset.editable_url ? (aiConfig.base_url ?? "") : "";
	aiConfig.wire_format = preset.wire_format;
	aiConfig.model = "";
	onsave();
}
</script>

<div class="field">
	<label for="ai-provider">Provider</label>
	<Select
		id="ai-provider"
		value={aiConfig.provider}
		options={BYO_PRESETS.map((p) => ({ value: p.id, label: p.label }))}
		onchange={handleProviderChange}
	/>
</div>

{#if showBaseUrl}
	<div class="field">
		<label for="ai-base-url">Endpoint</label>
		<input
			id="ai-base-url"
			class="control"
			type="text"
			bind:value={aiConfig.base_url}
			placeholder={currentPreset.base_url || "https://your-endpoint/v1/chat/completions"}
			spellcheck="false"
			onchange={onsave}
		/>
	</div>
{/if}

<div class="field">
	<label for="ai-model">Model</label>
	<input
		id="ai-model"
		class="control"
		type="text"
		bind:value={aiConfig.model}
		placeholder="model name…"
		spellcheck="false"
		onchange={onsave}
	/>
</div>
{#if currentPreset.model_hint}
	<div class="hint">{currentPreset.model_hint}</div>
{/if}

<ApiKeyField bind:this={apiKeyRef} provider={aiConfig.provider} {onhealthchange} />

<AdvancedFields bind:open={advancedOpen} bind:aiConfig {onsave} />

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
	.hint {
		font-size: 11px;
		color: var(--fg-muted);
		padding: 2px 0 6px 134px;
		line-height: 1.4;
	}
</style>

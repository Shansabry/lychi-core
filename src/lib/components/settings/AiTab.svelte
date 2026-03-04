<script lang="ts">
import type { AiConfig } from "$lib/ipc";
import { checkAiHealth, getMaskedApiKey, saveAiConfig, setApiKey } from "$lib/ipc";
import Select from "../Select.svelte";

let {
	aiConfig = $bindable(),
	onsaveerror,
}: {
	aiConfig: AiConfig;
	onsaveerror: (msg: string) => void;
} = $props();

let apiKeyInput = $state("");
let maskedKey: string | null = $state(null);
let editingKey = $state(false);
let confirmingClear = $state(false);
let healthStatus: "checking" | "healthy" | "error" | "disabled" = $state("disabled");
let saving = $state(false);

type ModelEntry = { value: string; label: string };
type ModelManifest = Record<string, ModelEntry[]>;

const MODELS_URL = "https://raw.githubusercontent.com/user/lychi/main/models.json";

const FALLBACK_MODELS: ModelManifest = {
	anthropic: [
		{ value: "claude-haiku-4-5-20251001", label: "$ Claude Haiku 4.5" },
		{ value: "claude-sonnet-4-5-20250929", label: "$$ Claude Sonnet 4.5" },
		{ value: "claude-opus-4-6", label: "$$$ Claude Opus 4.6" },
	],
	openai: [
		{ value: "gpt-4.1-nano", label: "$ GPT-4.1 Nano" },
		{ value: "gpt-4.1-mini", label: "$ GPT-4.1 Mini" },
		{ value: "gpt-4o-mini", label: "$ GPT-4o Mini" },
		{ value: "gpt-4o", label: "$$ GPT-4o" },
		{ value: "gpt-5.2", label: "$$$ GPT-5.2" },
	],
	groq: [
		{ value: "llama-3.1-8b-instant", label: "$ Llama 3.1 8B" },
		{ value: "llama-3.3-70b-versatile", label: "$ Llama 3.3 70B" },
		{ value: "mixtral-8x7b-32768", label: "$ Mixtral 8x7B" },
	],
};

let cachedManifest: ModelManifest | null = null;
let providerModels: ModelManifest = $state(FALLBACK_MODELS);
let models = $derived(providerModels[aiConfig.provider] ?? []);

export async function initModels(aiMode: string) {
	providerModels = await fetchModels(aiMode);
	refreshHealth();
	refreshMaskedKey(aiConfig.provider);
}

async function fetchModels(aiMode: string): Promise<ModelManifest> {
	if (cachedManifest) return cachedManifest;
	if (aiMode !== "disabled") {
		try {
			const res = await fetch(MODELS_URL, { signal: AbortSignal.timeout(3000) });
			if (!res.ok) throw new Error(`HTTP ${res.status}`);
			const data = await res.json();
			if (data.providers && typeof data.providers === "object") {
				const manifest: ModelManifest = data.providers;
				cachedManifest = manifest;
				return manifest;
			}
		} catch {
			// Offline or bad response — use fallback
		}
	}
	cachedManifest = FALLBACK_MODELS;
	return FALLBACK_MODELS;
}

async function refreshHealth() {
	if (aiConfig.mode === "disabled") {
		healthStatus = "disabled";
		return;
	}
	healthStatus = "checking";
	try {
		const ok = await checkAiHealth();
		healthStatus = ok ? "healthy" : "error";
	} catch {
		healthStatus = "error";
	}
}

async function refreshMaskedKey(provider: string) {
	try {
		maskedKey = await getMaskedApiKey(provider);
	} catch {
		maskedKey = null;
	}
}

async function saveAi() {
	saving = true;
	onsaveerror("");
	try {
		await saveAiConfig(aiConfig);
		await refreshHealth();
	} catch (err) {
		console.error("[settings] Failed to save AI config:", err);
		onsaveerror(`Failed to save: ${err}`);
	} finally {
		saving = false;
	}
}

async function handleModeChange(val: string) {
	aiConfig.mode = val;
	await saveAi();
	if (val !== "disabled") {
		cachedManifest = null;
		providerModels = await fetchModels(val);
	}
}

async function handleProviderChange(val: string) {
	aiConfig.provider = val;
	const available = providerModels[aiConfig.provider];
	aiConfig.model = available?.[0]?.value ?? "";
	apiKeyInput = "";
	editingKey = false;
	confirmingClear = false;
	await saveAi();
	refreshMaskedKey(val);
}

async function handleModelChange(val: string) {
	aiConfig.model = val;
	await saveAi();
}

async function handleSetApiKey() {
	if (!apiKeyInput.trim()) return;
	saving = true;
	try {
		await setApiKey(aiConfig.provider, apiKeyInput.trim());
		apiKeyInput = "";
		editingKey = false;
		confirmingClear = false;
		await refreshHealth();
		await refreshMaskedKey(aiConfig.provider);
	} finally {
		saving = false;
	}
}

async function handleClearApiKey() {
	if (!confirmingClear) {
		confirmingClear = true;
		return;
	}
	confirmingClear = false;
	saving = true;
	try {
		await setApiKey(aiConfig.provider, "");
		maskedKey = null;
		apiKeyInput = "";
		editingKey = false;
		await refreshHealth();
	} finally {
		saving = false;
	}
}

export function dismissConfirm() {
	if (confirmingClear) {
		confirmingClear = false;
		return true;
	}
	return false;
}
</script>

<div class="field">
	<label for="ai-mode">Mode</label>
	<Select
		id="ai-mode"
		value={aiConfig.mode}
		options={[
			{ value: "disabled", label: "Disabled" },
			{ value: "byo", label: "BYO API Key" },
		]}
		onchange={handleModeChange}
	/>
</div>

{#if aiConfig.mode === "byo"}
	<div class="field">
		<label for="ai-provider">Provider</label>
		<Select
			id="ai-provider"
			value={aiConfig.provider}
			options={[
				{ value: "anthropic", label: "Anthropic" },
				{ value: "openai", label: "OpenAI" },
				{ value: "groq", label: "Groq" },
			]}
			onchange={handleProviderChange}
		/>
	</div>

	<div class="field">
		<label for="ai-model">Model</label>
		<Select
			id="ai-model"
			value={aiConfig.model}
			options={models}
			onchange={handleModelChange}
		/>
	</div>

	<div class="field">
		<label for="ai-timeout">Timeout</label>
		<div class="number-row">
			<input
				id="ai-timeout"
				type="number"
				min="2"
				max="60"
				bind:value={aiConfig.timeout_secs}
				onchange={saveAi}
			/>
			<span class="unit-label">s</span>
		</div>
	</div>

	<div class="field">
		<label for="ai-max-tokens">Max Tokens</label>
		<input
			id="ai-max-tokens"
			type="number"
			min="100"
			max="2000"
			bind:value={aiConfig.max_tokens}
			onchange={saveAi}
		/>
	</div>

	<div class="field">
		<label for="ai-key">API Key</label>
		<div class="key-row">
			{#if maskedKey && !editingKey}
				<span class="masked-key">{maskedKey}</span>
				<button class="set-btn" onclick={() => { editingKey = true; confirmingClear = false; }} disabled={saving}>
					Change
				</button>
				<button class="set-btn clear-btn" class:confirming={confirmingClear} onclick={handleClearApiKey} disabled={saving}>
					{confirmingClear ? "Sure?" : "Clear"}
				</button>
			{:else}
				<input
					id="ai-key"
					type="password"
					bind:value={apiKeyInput}
					placeholder="Enter API key..."
					spellcheck="false"
					onkeydown={(e) => { if (e.key === "Enter") { e.preventDefault(); handleSetApiKey(); } }}
				/>
				<button class="set-btn" onclick={handleSetApiKey} disabled={saving || !apiKeyInput.trim()}>
					Set
				</button>
				{#if maskedKey}
					<button class="set-btn" onclick={() => { editingKey = false; apiKeyInput = ""; confirmingClear = false; }} disabled={saving}>
						Cancel
					</button>
				{/if}
			{/if}
		</div>
	</div>

	<div class="field">
		<span class="field-label">Status</span>
		<div class="health-status">
			<span
				class="health-dot"
				class:healthy={healthStatus === "healthy"}
				class:error={healthStatus === "error"}
				class:checking={healthStatus === "checking"}
			></span>
			<span class="health-label">
				{#if healthStatus === "checking"}
					Checking...
				{:else if healthStatus === "healthy"}
					Connected
				{:else if healthStatus === "error"}
					Not connected
				{:else}
					Disabled
				{/if}
			</span>
		</div>
	</div>
{/if}

<style>
	.field {
		display: flex;
		align-items: center;
		justify-content: space-between;
		padding: 6px 0;
		gap: 12px;
	}

	label,
	.field-label {
		color: var(--fg-muted);
		font-size: 12px;
		flex-shrink: 0;
		width: 120px;
	}

	input[type="number"] {
		background: var(--bg-secondary);
		color: var(--fg);
		border: 1px solid var(--border);
		border-radius: 4px;
		padding: 5px 8px;
		font-family: var(--font-mono);
		font-size: 12px;
		outline: none;
		width: 64px;
		text-align: right;
	}

	input[type="number"]:focus {
		border-color: var(--fg-muted);
	}

	.number-row {
		display: flex;
		align-items: center;
		gap: 4px;
	}

	.unit-label {
		font-size: 12px;
		color: var(--fg-muted);
	}

	.key-row {
		display: flex;
		gap: 6px;
		flex: 1;
		min-width: 0;
		align-items: center;
	}

	.key-row input {
		flex: 1;
		min-width: 0;
	}

	input[type="password"] {
		background: var(--bg-secondary);
		color: var(--fg);
		border: 1px solid var(--border);
		border-radius: 4px;
		padding: 5px 8px;
		font-family: var(--font-mono);
		font-size: 12px;
		outline: none;
		flex: 1;
		min-width: 0;
	}

	input[type="password"]:focus {
		border-color: var(--fg-muted);
	}

	.masked-key {
		font-family: var(--font-mono);
		font-size: 12px;
		color: var(--fg-muted);
		letter-spacing: 0.02em;
		flex: 1;
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.set-btn {
		background: var(--bg-secondary);
		color: var(--accent);
		border: 1px solid var(--border);
		border-radius: 4px;
		padding: 5px 12px;
		font-family: var(--font-mono);
		font-size: 12px;
		cursor: pointer;
		flex-shrink: 0;
		transition: background 100ms ease;
	}

	.set-btn:hover:not(:disabled) {
		background: var(--border);
	}

	.set-btn:disabled {
		opacity: 0.4;
		cursor: default;
	}

	.clear-btn:hover {
		color: var(--error);
		border-color: var(--error);
		background: var(--bg-secondary);
	}

	.clear-btn.confirming {
		color: var(--error);
		border-color: var(--error);
	}

	.health-status {
		display: flex;
		align-items: center;
		gap: 6px;
	}

	.health-dot {
		width: 8px;
		height: 8px;
		border-radius: 50%;
		background: #666;
		flex-shrink: 0;
	}

	.health-dot.healthy {
		background: #44ff44;
	}

	.health-dot.error {
		background: #ff4444;
	}

	.health-dot.checking {
		background: #666;
		animation: pulse 1s ease-in-out infinite;
	}

	.health-label {
		font-size: 12px;
		color: var(--fg-muted);
	}

	@keyframes pulse {
		0%, 100% { opacity: 1; }
		50% { opacity: 0.4; }
	}
</style>

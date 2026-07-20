<script lang="ts">
import { listen } from "@tauri-apps/api/event";
import { onMount } from "svelte";
import type { AiConfig, CreditBalance, FirebaseUser, OllamaModelInfo } from "$lib/ipc";
import {
	checkAiHealth,
	cloudGetCredits,
	firebaseGetUser,
	firebaseSignIn,
	firebaseSignOut,
	getMaskedApiKey,
	listOllamaModels,
	saveAiConfig,
	setApiKey,
} from "$lib/ipc";
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

// Ollama state
let ollamaModels: OllamaModelInfo[] = $state([]);
let ollamaFetchError: string | null = $state(null);

// Lychi Cloud is disabled for launch (BYOK + Ollama only) — flip this when
// lychi-cloud ships (Phase 2.3). Typed as boolean so TS doesn't narrow the
// gated markup to unreachable.
const CLOUD_ENABLED: boolean = false;

// Cloud state
let cloudUser: FirebaseUser | null = $state(null);
let cloudCredits: CreditBalance | null = $state(null);
let cloudLoading = $state(false);

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

onMount(() => {
	initModels(aiConfig.mode);

	// Listen for deep-link auth callback — refresh cloud user when sign-in completes
	const unlistenSignIn = listen("lychi://firebase-signed-in", () => {
		refreshCloudUser();
	});
	const unlistenSignOut = listen("lychi://firebase-signed-out", () => {
		cloudUser = null;
		cloudCredits = null;
	});

	return () => {
		unlistenSignIn.then((u) => u());
		unlistenSignOut.then((u) => u());
	};
});

export async function initModels(aiMode: string) {
	if (aiMode === "ollama") {
		fetchOllamaModels();
	} else if (aiMode === "cloud") {
		if (CLOUD_ENABLED) refreshCloudUser();
	} else {
		providerModels = await fetchModels(aiMode);
		refreshMaskedKey(aiConfig.provider);
	}
	refreshHealth();
}

async function refreshCloudUser() {
	try {
		cloudUser = await firebaseGetUser();
		if (cloudUser) {
			refreshCloudCredits();
		} else {
			cloudCredits = null;
		}
	} catch {
		cloudUser = null;
		cloudCredits = null;
	}
}

async function refreshCloudCredits() {
	try {
		cloudCredits = await cloudGetCredits();
	} catch {
		cloudCredits = null;
	}
}

async function handleCloudSignIn() {
	cloudLoading = true;
	try {
		await firebaseSignIn();
	} catch (err) {
		onsaveerror(`Sign in failed: ${err}`);
	} finally {
		cloudLoading = false;
	}
}

async function handleCloudSignOut() {
	cloudLoading = true;
	try {
		await firebaseSignOut();
		cloudUser = null;
		cloudCredits = null;
	} catch (err) {
		onsaveerror(`Sign out failed: ${err}`);
	} finally {
		cloudLoading = false;
	}
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

async function fetchOllamaModels() {
	ollamaFetchError = null;
	try {
		ollamaModels = await listOllamaModels();
		if (ollamaModels.length > 0 && !aiConfig.ollama_model) {
			aiConfig.ollama_model = ollamaModels[0].name;
			saveAi();
		}
	} catch (e) {
		ollamaModels = [];
		ollamaFetchError = `Cannot connect to Ollama at ${aiConfig.ollama_url}`;
	}
}

function formatSize(bytes: number): string {
	if (bytes >= 1e9) return `${(bytes / 1e9).toFixed(1)} GB`;
	if (bytes >= 1e6) return `${(bytes / 1e6).toFixed(0)} MB`;
	return `${bytes} B`;
}

async function handleModeChange(val: string) {
	aiConfig.mode = val;
	await saveAi();
	if (val === "ollama") {
		fetchOllamaModels();
	} else if (val === "cloud") {
		if (CLOUD_ENABLED) refreshCloudUser();
	} else if (val !== "disabled") {
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

async function handleOllamaModelChange(val: string) {
	aiConfig.ollama_model = val;
	await saveAi();
}

async function handleOllamaUrlChange() {
	await saveAi();
	fetchOllamaModels();
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
			// Lychi Cloud is hidden until lychi-cloud ships (Phase 2.3) —
			// launch supports BYOK + Ollama only. The entry below only appears
			// if an existing config still has mode="cloud".
			...(aiConfig.mode === "cloud"
				? [{ value: "cloud", label: "Lychi Cloud (coming soon)" }]
				: []),
			{ value: "ollama", label: "Ollama (Local)" },
			{ value: "byo", label: "BYO API Key" },
		]}
		onchange={handleModeChange}
	/>
</div>

{#if aiConfig.mode === "cloud"}
	<div class="field-hint">
		Lychi Cloud isn't available yet — switch to Ollama (local) or BYO API key.
	</div>
{/if}
{#if CLOUD_ENABLED && aiConfig.mode === "cloud"}
	{#if cloudUser}
		<div class="field">
			<span class="field-label">Signed in as</span>
			<span class="cloud-email">{cloudUser.email}</span>
		</div>

		{#if cloudCredits}
			<div class="field">
				<span class="field-label">Credits</span>
				<div class="credit-info">
					<span class="credit-balance">{cloudCredits.balance.toLocaleString()}</span>
					<span class="credit-meta">/ {cloudCredits.plan} plan</span>
				</div>
			</div>
			{#if cloudCredits.bonus_pool > 0}
				<div class="field">
					<span class="field-label">Bonus pool</span>
					<span class="credit-meta">{cloudCredits.bonus_pool.toLocaleString()}</span>
				</div>
			{/if}
		{/if}

		<div class="field">
			<span class="field-label"></span>
			<button class="set-btn clear-btn" onclick={handleCloudSignOut} disabled={cloudLoading}>
				Sign out
			</button>
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
	{:else}
		<div class="field">
			<span class="field-label">Account</span>
			<button class="set-btn" onclick={handleCloudSignIn} disabled={cloudLoading}>
				{cloudLoading ? "Opening browser..." : "Sign in with Google"}
			</button>
		</div>
		<div class="cloud-hint">
			Opens your browser to sign in. You'll be redirected back to Lychi automatically.
		</div>
	{/if}
{:else if aiConfig.mode === "ollama"}
	<div class="field">
		<label for="ollama-url">URL</label>
		<input
			id="ollama-url"
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
				options={ollamaModels.map(m => ({
					value: m.name,
					label: `${m.name} — ${formatSize(m.size)}`,
				}))}
				onchange={handleOllamaModelChange}
			/>
		{:else if ollamaFetchError}
			<span class="ollama-hint error">{ollamaFetchError}</span>
		{:else}
			<span class="ollama-hint">No models found — run <code>ollama pull &lt;model&gt;</code></span>
		{/if}
	</div>

	<div class="field">
		<label for="ollama-timeout">Timeout</label>
		<div class="number-row">
			<input
				id="ollama-timeout"
				type="number"
				min="2"
				max="120"
				bind:value={aiConfig.timeout_secs}
				onchange={saveAi}
			/>
			<span class="unit-label">s</span>
		</div>
	</div>

	<div class="field">
		<label for="ollama-max-tokens">Max Tokens</label>
		<input
			id="ollama-max-tokens"
			type="number"
			min="100"
			max="4000"
			bind:value={aiConfig.max_tokens}
			onchange={saveAi}
		/>
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
{:else if aiConfig.mode === "byo"}
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

	input[type="text"] {
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

	input[type="text"]:focus {
		border-color: var(--fg-muted);
	}

	.ollama-hint {
		font-size: 11px;
		color: var(--fg-muted);
	}

	.cloud-email {
		font-family: var(--font-mono);
		font-size: 12px;
		color: var(--fg);
	}

	.credit-info {
		display: flex;
		align-items: baseline;
		gap: 6px;
	}

	.credit-balance {
		font-family: var(--font-mono);
		font-size: 14px;
		color: var(--accent);
		font-weight: 600;
	}

	.credit-meta {
		font-size: 11px;
		color: var(--fg-muted);
	}

	.cloud-hint {
		font-size: 11px;
		color: var(--fg-muted);
		padding: 4px 0 8px 132px;
		line-height: 1.5;
	}

	.ollama-hint.error {
		color: var(--error);
	}

	.ollama-hint code {
		font-family: var(--font-mono);
		background: var(--bg-secondary);
		padding: 1px 4px;
		border-radius: 2px;
	}
</style>

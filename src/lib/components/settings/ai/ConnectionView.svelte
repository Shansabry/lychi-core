<script lang="ts">
import type { AiConfig, CommandsConfig } from "$lib/ipc";
import {
	checkAiHealth,
	getMaskedApiKey,
	saveAiConfig,
	saveCommandsConfig,
	testAiConnection,
} from "$lib/ipc";
import Select from "../../Select.svelte";
import type ApiKeyField from "./ApiKeyField.svelte";
import ConnectionStatus from "./ConnectionStatus.svelte";
import ConnectionSummary from "./ConnectionSummary.svelte";
import ByoFields from "./modes/ByoFields.svelte";
import CloudPanel from "./modes/CloudPanel.svelte";
import LocalModels from "./modes/LocalModels.svelte";
import OllamaFields from "./modes/OllamaFields.svelte";
import PotentialMeter from "./PotentialMeter.svelte";

let {
	aiConfig = $bindable(),
	commandsConfig = $bindable(),
	onsaveerror,
}: {
	aiConfig: AiConfig;
	commandsConfig: CommandsConfig;
	onsaveerror: (msg: string) => void;
} = $props();

// The agent's shell approval profile lives in commandsConfig.shell_policy.
// Defaults to "ask-on-write" (today's behaviour) when the config predates it.
let shellProfile = $derived(commandsConfig.shell_policy?.profile ?? "ask-on-write");

async function handleShellProfileChange(val: string) {
	commandsConfig.shell_policy = {
		...(commandsConfig.shell_policy ?? { allow: [], deny: [] }),
		profile: val as typeof shellProfile,
	};
	try {
		await saveCommandsConfig(commandsConfig);
	} catch (err) {
		console.error("[settings] Failed to save shell policy:", err);
		onsaveerror(`Failed to save: ${err}`);
	}
}

// Lychi Cloud is disabled for launch (flip when lychi-cloud ships, Phase 2.3).
const CLOUD_ENABLED: boolean = false;

type Health = "checking" | "healthy" | "error" | "disabled";
let healthStatus: Health = $state<Health>("disabled");
let testing = $state(false);
let testResult: { ok: boolean; error: string | null } | null = $state(null);
let saving = $state(false);

// Summary (collapsed) vs editing (form). Editing when the user asks to change,
// or when there's no working config yet.
let editing = $state(false);
let advancedOpen = $state(false);
let summaryMaskedKey: string | null = $state(null);

// Child refs for keyboard-driven actions + confirm dismissal.
let ollamaRef: OllamaFields | undefined = $state();
let localRef: LocalModels | undefined = $state();
let cloudRef: CloudPanel | undefined = $state();
let apiKeyRef: ApiKeyField | undefined = $state();

const MODE_LABELS: Record<string, string> = {
	disabled: "Disabled",
	cloud: "Lychi Cloud",
	local: "Local AI",
	ollama: "Ollama",
	byo: "BYO API Key",
};

const PROVIDER_LABELS: Record<string, string> = {
	anthropic: "Anthropic",
	openai: "OpenAI",
	groq: "Groq",
	grok: "Grok (xAI)",
	gemini: "Gemini (Google)",
	openrouter: "OpenRouter",
	custom: "Custom",
};

const providerLabel = $derived(
	aiConfig.mode === "byo"
		? (PROVIDER_LABELS[aiConfig.provider] ?? aiConfig.provider)
		: (MODE_LABELS[aiConfig.mode] ?? aiConfig.mode),
);

const summaryModel = $derived(
	aiConfig.mode === "ollama"
		? aiConfig.ollama_model
		: aiConfig.mode === "local"
			? aiConfig.local_model
			: aiConfig.model,
);

// The summary is shown only when we have a healthy config AND aren't editing.
const showSummary = $derived(
	!editing && aiConfig.mode !== "disabled" && healthStatus === "healthy",
);

export async function initModels(aiMode: string) {
	if (aiMode === "ollama") {
		ollamaRef?.fetchOllamaModels();
	} else if (aiMode === "cloud") {
		if (CLOUD_ENABLED) cloudRef?.refreshCloudUser();
	} else if (aiMode === "byo") {
		refreshSummaryKey();
	} else if (aiMode === "local") {
		localRef?.refreshLocalModels();
	}
	await refreshHealth();
}

async function refreshSummaryKey() {
	try {
		summaryMaskedKey = await getMaskedApiKey(aiConfig.provider);
	} catch {
		summaryMaskedKey = null;
	}
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

export async function runConnectionTest() {
	testing = true;
	testResult = null;
	try {
		// Persist first so the backend tests exactly what's on screen.
		await saveAiConfig(aiConfig);
		testResult = await testAiConnection();
		healthStatus = testResult.ok ? "healthy" : "error";
	} catch (err) {
		testResult = { ok: false, error: `${err}` };
		healthStatus = "error";
	} finally {
		testing = false;
	}
}

async function handleModeChange(val: string) {
	testResult = null;
	aiConfig.mode = val;
	editing = true;
	await saveAi();
	if (val === "ollama") {
		ollamaRef?.fetchOllamaModels();
	} else if (val === "cloud") {
		if (CLOUD_ENABLED) cloudRef?.refreshCloudUser();
	} else if (val === "byo") {
		refreshSummaryKey();
	} else if (val === "local") {
		localRef?.refreshLocalModels();
	}
}

function onhealthchange() {
	refreshHealth();
	refreshSummaryKey();
}

export function toggleAdvanced() {
	advancedOpen = !advancedOpen;
}

export function moveModelSelection(delta: number) {
	if (aiConfig.mode === "local") localRef?.moveSelection(delta);
}

export function activateModel() {
	if (aiConfig.mode === "local") localRef?.activateSelected();
}

export function removeModel() {
	if (aiConfig.mode === "local") localRef?.removeSelected();
}

export function dismissConfirm(): boolean {
	return apiKeyRef?.dismissConfirm() ?? false;
}
</script>

{#if showSummary}
	<ConnectionSummary
		{providerLabel}
		model={summaryModel}
		maskedKey={aiConfig.mode === "byo" ? summaryMaskedKey : null}
		modeLabel={MODE_LABELS[aiConfig.mode] ?? aiConfig.mode}
		health={healthStatus}
		{testing}
		onchangerequest={() => (editing = true)}
		ontest={runConnectionTest}
	/>
{:else}
	<div class="field">
		<label for="ai-mode">Mode</label>
		<Select
			id="ai-mode"
			value={aiConfig.mode}
			options={[
				{ value: "disabled", label: "Disabled" },
				{
					value: "cloud",
					label: "Lychi Cloud (coming soon)",
					disabled: !CLOUD_ENABLED && aiConfig.mode !== "cloud",
				},
				{ value: "local", label: "Local AI (bundled)" },
				{ value: "ollama", label: "Ollama (Local)" },
				{ value: "byo", label: "BYO API Key" },
			]}
			onchange={handleModeChange}
		/>
	</div>

	{#if aiConfig.mode === "cloud"}
		<CloudPanel bind:this={cloudRef} {onsaveerror} />
		<ConnectionStatus
			health={healthStatus}
			{testing}
			{testResult}
			ontest={runConnectionTest}
		/>
	{:else if aiConfig.mode === "local"}
		<div class="intro">
			Private, offline AI that runs on your CPU — nothing leaves your machine. Pick a
			model to download.
		</div>
		<LocalModels
			bind:this={localRef}
			bind:aiConfig
			bind:advancedOpen
			onsave={saveAi}
			{onsaveerror}
		/>
		<ConnectionStatus
			health={healthStatus}
			{testing}
			{testResult}
			ontest={runConnectionTest}
		/>
	{:else if aiConfig.mode === "ollama"}
		<OllamaFields bind:this={ollamaRef} bind:aiConfig bind:advancedOpen onsave={saveAi} />
		<ConnectionStatus
			health={healthStatus}
			{testing}
			{testResult}
			ontest={runConnectionTest}
		/>
	{:else if aiConfig.mode === "byo"}
		<ByoFields
			bind:aiConfig
			bind:advancedOpen
			bind:apiKeyRef
			onsave={saveAi}
			{onhealthchange}
		/>
		<ConnectionStatus
			health={healthStatus}
			{testing}
			{testResult}
			ontest={runConnectionTest}
		/>
	{/if}

	<!-- Capability meter (all modes). Shows the estimated tier and, when the tier
	     is Basic, the "experimental / expect simpler reasoning" caveat. Refreshes
	     whenever the active model/mode changes. -->
	{#if aiConfig.mode !== "disabled"}
		<PotentialMeter refreshKey={`${aiConfig.mode}:${summaryModel}`} />
	{/if}

	<!-- Agent safety: how much the AI must ask before running a shell command.
	     Lives on the AI page because it governs the AI agent's behaviour. -->
	<div class="agent-safety">
		<div class="section-label">Agent safety</div>
		<div class="field">
			<label for="shell-profile">Command approval</label>
			<Select
				id="shell-profile"
				value={shellProfile}
				options={[
					{ value: "strict", label: "Strict — confirm every command" },
					{ value: "ask-on-write", label: "Ask before changes (recommended)" },
					{ value: "auto-accept", label: "Auto-accept (only blocks the denylist)" },
				]}
				onchange={handleShellProfileChange}
			/>
		</div>
		<div class="hint">
			When the AI must ask before running a shell command. Catastrophic commands
			(wiping the disk, piping a download into a shell) are always blocked,
			whatever the profile.
		</div>
	</div>
{/if}

<style>
	.field {
		display: grid;
		grid-template-columns: 120px 1fr;
		align-items: center;
		gap: 14px;
		padding: 9px 0 16px;
	}
	label {
		font-size: 12.5px;
		color: var(--fg-muted);
	}
	.intro {
		font-size: 12px;
		color: var(--fg-muted);
		line-height: 1.5;
		padding: 4px 0 10px;
	}
	.agent-safety {
		margin-top: 8px;
		padding-top: 14px;
		border-top: 1px solid color-mix(in srgb, var(--border) 60%, transparent);
	}
	.section-label {
		font-size: 11px;
		text-transform: uppercase;
		letter-spacing: 0.06em;
		color: var(--fg-muted);
		margin-bottom: 6px;
	}
	.hint {
		font-size: 11.5px;
		color: var(--fg-muted);
		line-height: 1.5;
	}
</style>

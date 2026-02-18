<script lang="ts">
import { getVersion } from "@tauri-apps/api/app";
import {
	BookOpen,
	Check,
	ChevronRight,
	FolderOpen,
	Info,
	Moon,
	SlidersHorizontal,
	Sparkles,
	Sun,
	X,
} from "lucide-svelte";
import { onMount } from "svelte";
import type { AiConfig, CommandsConfig, DirEntry, GeneralConfig, ProjectsConfig } from "$lib/ipc";
import {
	checkAiHealth,
	getAiConfig,
	getCommandsConfig,
	getGeneralConfig,
	getProjectsConfig,
	listDirectories,
	recordHotkey,
	saveAiConfig,
	saveCommandsConfig,
	saveGeneralConfig,
	saveProjectsConfig,
	setApiKey,
	setHotkey,
} from "$lib/ipc";
import Select from "./Select.svelte";

let { ondismiss }: { ondismiss: () => void } = $props();

let activeTab: "general" | "ai" | "projects" | "guide" | "about" = $state("general");
let guideTab: "shortcuts" | "commands" | "triggers" = $state("shortcuts");
let appVersion = $state("");

let aiConfig: AiConfig = $state({
	mode: "disabled",
	provider: "anthropic",
	model: "",
	ollama_url: "",
});
let generalConfig: GeneralConfig = $state({
	hide_on_blur: true,
	show_duration_ms: true,
	theme: "dark",
	hotkey: "Super+Space",
	window_x: null,
	window_y: null,
});
let commandsConfig: CommandsConfig = $state({
	default_search_engine: "https://www.google.com/search?q=",
	youtube_url: "https://www.youtube.com/results?search_query=",
	shell: "/bin/bash",
});

let apiKeyInput = $state("");
let healthStatus: "checking" | "healthy" | "error" | "disabled" = $state("disabled");
let saving = $state(false);
let hotkeyError = $state("");
let recordingHotkey = $state(false);
let customShell = $state(false);

const knownShells = [
	"/bin/bash",
	"/bin/zsh",
	"/bin/fish",
	"/bin/sh",
	"/usr/bin/nu",
	"/usr/bin/pwsh",
];
let shellOptions = $derived([
	{ value: "/bin/bash", label: "Bash" },
	{ value: "/bin/zsh", label: "Zsh" },
	{ value: "/bin/fish", label: "Fish" },
	{ value: "/bin/sh", label: "sh" },
	{ value: "/usr/bin/nu", label: "Nushell" },
	{ value: "/usr/bin/pwsh", label: "PowerShell" },
	{ value: "__custom__", label: "Custom..." },
]);

let projectDirs: string[] = $state([]);

onMount(async () => {
	const [ai, general, commands, projects, manifest, version] = await Promise.all([
		getAiConfig(),
		getGeneralConfig(),
		getCommandsConfig(),
		getProjectsConfig(),
		fetchModels(),
		getVersion().catch(() => "0.0.0"),
	]);
	aiConfig = ai;
	generalConfig = general;
	commandsConfig = commands;
	customShell = !knownShells.includes(commands.shell);
	projectDirs = projects.directories;
	providerModels = manifest;
	appVersion = version;
	await refreshHealth();
});

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

let saveError = $state("");

async function saveAi() {
	saving = true;
	saveError = "";
	try {
		await saveAiConfig(aiConfig);
		await refreshHealth();
	} catch (err) {
		console.error("[settings] Failed to save AI config:", err);
		saveError = `Failed to save: ${err}`;
	} finally {
		saving = false;
	}
}

async function handleModeChange(val: string) {
	aiConfig.mode = val;
	await saveAi();
}

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

async function fetchModels(): Promise<ModelManifest> {
	if (cachedManifest) return cachedManifest;
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
	cachedManifest = FALLBACK_MODELS;
	return FALLBACK_MODELS;
}

let providerModels: ModelManifest = $state(FALLBACK_MODELS);

let models = $derived(providerModels[aiConfig.provider] ?? []);

async function handleProviderChange(val: string) {
	aiConfig.provider = val;
	const available = providerModels[aiConfig.provider];
	aiConfig.model = available?.[0]?.value ?? "";
	await saveAi();
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
		await refreshHealth();
	} finally {
		saving = false;
	}
}

async function handleHideOnBlurToggle() {
	generalConfig.hide_on_blur = !generalConfig.hide_on_blur;
	try {
		await saveGeneralConfig(generalConfig);
	} catch (err) {
		console.error("[settings] Failed to save general config:", err);
		saveError = `Failed to save: ${err}`;
	}
}

async function handleThemeChange(val: string) {
	generalConfig.theme = val;
	document.documentElement.dataset.theme = generalConfig.theme;
	window.dispatchEvent(new CustomEvent("lychi-theme-change", { detail: generalConfig.theme }));
	try {
		await saveGeneralConfig(generalConfig);
	} catch (err) {
		console.error("[settings] Failed to save theme:", err);
		saveError = `Failed to save: ${err}`;
	}
}

function handleShellSelect(val: string) {
	if (val === "__custom__") {
		customShell = true;
		commandsConfig.shell = "";
		return;
	}
	handleShellChange(val);
}

async function handleShellChange(val: string) {
	if (!val.trim()) return;
	commandsConfig.shell = val.trim();
	try {
		await saveCommandsConfig(commandsConfig);
	} catch (err) {
		console.error("[settings] Failed to save shell config:", err);
		saveError = `Failed to save: ${err}`;
	}
}

async function saveProjectDirs() {
	try {
		await saveProjectsConfig({ directories: projectDirs });
	} catch (err) {
		console.error("[settings] Failed to save projects config:", err);
		saveError = `Failed to save: ${err}`;
	}
}

let browsing = $state(false);
let browsePath = $state("");
let browseDirs: DirEntry[] = $state([]);

async function openBrowser() {
	browsing = true;
	browseInput = "";
	browsePath = "";
	browseDirs = await listDirectories("");
}

async function navigateTo(path: string) {
	browsePath = path;
	browseInput = path;
	browseDirs = await listDirectories(path);
}

async function goUp() {
	if (!browsePath || browsePath === "/") return;
	const parent = browsePath.replace(/\/[^/]+\/?$/, "") || "/";
	await navigateTo(parent);
}

async function handleBrowseInput() {
	const val = browseInput.trim();
	if (!val) {
		await navigateTo("");
		return;
	}
	browsePath = val;
	browseDirs = await listDirectories(val);
}

function selectCurrentDir() {
	const selected = browsePath || "/";
	if (!projectDirs.includes(selected)) {
		projectDirs = [...projectDirs, selected];
		saveProjectDirs();
	}
	browsing = false;
}

let browseInput = $state("");

function removeProjectDir(index: number) {
	projectDirs = projectDirs.filter((_, i) => i !== index);
	saveProjectDirs();
}

function handleKeydown(e: KeyboardEvent) {
	if (e.key === "Escape") {
		e.preventDefault();
		e.stopPropagation();
		ondismiss();
	}
}
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="settings-panel" onkeydown={handleKeydown}>
	<nav class="sidebar">
		<button
			class="tab-btn"
			class:active={activeTab === "general"}
			onclick={() => (activeTab = "general")}
		>
			<SlidersHorizontal size={14} strokeWidth={1.5} />
			<span>General</span>
		</button>
		<button
			class="tab-btn"
			class:active={activeTab === "ai"}
			onclick={() => (activeTab = "ai")}
		>
			<Sparkles size={14} strokeWidth={1.5} />
			<span>AI</span>
		</button>
		<button
			class="tab-btn"
			class:active={activeTab === "projects"}
			onclick={() => (activeTab = "projects")}
		>
			<FolderOpen size={14} strokeWidth={1.5} />
			<span>Projects</span>
		</button>
		<button
			class="tab-btn"
			class:active={activeTab === "guide"}
			onclick={() => (activeTab = "guide")}
		>
			<BookOpen size={14} strokeWidth={1.5} />
			<span>Guide</span>
		</button>
		<button
			class="tab-btn about-tab"
			class:active={activeTab === "about"}
			onclick={() => (activeTab = "about")}
		>
			<Info size={14} strokeWidth={1.5} />
			<span>About</span>
		</button>
	</nav>

	<div class="content">
		{#if activeTab === "general"}
			<div class="field">
				<span class="field-label">Hotkey</span>
				<div class="hotkey-row">
					<button
						class="hotkey-btn"
						class:recording={recordingHotkey}
						onclick={async () => {
							if (recordingHotkey) return;
							recordingHotkey = true;
							hotkeyError = "";
							try {
								const combo = await recordHotkey();
								await setHotkey(combo);
								generalConfig.hotkey = combo;
							} catch (err) {
								const msg = String(err);
								if (!msg.includes("Cancelled")) {
									hotkeyError = msg;
								}
							} finally {
								recordingHotkey = false;
							}
						}}
					>
						{#if recordingHotkey}
							Press keys...
						{:else}
							{generalConfig.hotkey}
						{/if}
					</button>
				</div>
			</div>
			{#if hotkeyError}
				<div class="field-error">{hotkeyError}</div>
			{/if}
			<div class="field">
				<span class="field-label">Theme</span>
				<div class="theme-toggle">
					<button
						class="theme-option"
						class:active={generalConfig.theme === "dark"}
						onclick={() => handleThemeChange("dark")}
						title="Dark"
					>
						<Moon size={14} />
					</button>
					<button
						class="theme-option"
						class:active={generalConfig.theme === "light"}
						onclick={() => handleThemeChange("light")}
						title="Light"
					>
						<Sun size={14} />
					</button>
				</div>
			</div>
			<div class="field">
				<label for="hide-blur">Hide on blur</label>
				<button
					id="hide-blur"
					class="checkbox"
					class:checked={generalConfig.hide_on_blur}
					onclick={handleHideOnBlurToggle}
					role="checkbox"
					aria-checked={generalConfig.hide_on_blur}
				>
					{#if generalConfig.hide_on_blur}
						<svg width="12" height="12" viewBox="0 0 12 12" fill="none">
							<path d="M2 6L5 9L10 3" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"/>
						</svg>
					{/if}
				</button>
			</div>
			<div class="field">
				<label for="shell-select">Shell</label>
				{#if customShell}
					<div class="key-row">
						<input
							id="shell-input"
							type="text"
							bind:value={commandsConfig.shell}
							spellcheck="false"
							placeholder="/path/to/shell"
							onkeydown={(e) => { if (e.key === "Enter") { e.preventDefault(); handleShellChange(commandsConfig.shell); } }}
						/>
						<button class="set-btn" onclick={() => handleShellChange(commandsConfig.shell)}>Set</button>
						<button class="set-btn" onclick={() => { customShell = false; commandsConfig.shell = "/bin/bash"; handleShellChange("/bin/bash"); }} title="Back to list">
							<X size={12} strokeWidth={2} />
						</button>
					</div>
				{:else}
					<Select
						id="shell-select"
						value={commandsConfig.shell}
						options={shellOptions}
						onchange={handleShellSelect}
					/>
				{/if}
			</div>
		{:else if activeTab === "ai"}
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
					<label for="ai-key">API Key</label>
					<div class="key-row">
						<input
							id="ai-key"
							type="password"
							bind:value={apiKeyInput}
							placeholder="Enter key..."
							spellcheck="false"
							onkeydown={(e) => { if (e.key === "Enter") { e.preventDefault(); handleSetApiKey(); } }}
						/>
						<button class="set-btn" onclick={handleSetApiKey} disabled={saving || !apiKeyInput.trim()}>
							Set
						</button>
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
		{:else if activeTab === "projects"}
			{#if browsing}
				<div class="browser-header">
					<button class="browser-up" onclick={goUp} disabled={!browsePath || browsePath === "/"} title="Go up">
						..
					</button>
					<input
						class="browser-input"
						type="text"
						bind:value={browseInput}
						placeholder="/ or ~/path..."
						spellcheck="false"
						onkeydown={(e) => { if (e.key === "Enter") { e.preventDefault(); handleBrowseInput(); } }}
					/>
					<button class="browser-select" onclick={selectCurrentDir} title="Select this folder">
						<Check size={12} strokeWidth={2} />
					</button>
					<button class="browser-close" onclick={() => (browsing = false)} title="Cancel">
						<X size={12} strokeWidth={2} />
					</button>
				</div>
				<div class="browser-list">
					{#each browseDirs as dir}
						<button class="browser-item" onclick={() => navigateTo(dir.path)}>
							<FolderOpen size={12} strokeWidth={1.5} />
							<span>{dir.name}</span>
							<ChevronRight size={12} strokeWidth={1.5} />
						</button>
					{:else}
						<div class="browser-empty">No subdirectories</div>
					{/each}
				</div>
			{:else}
				<div class="dir-list">
					{#each projectDirs as dir, i}
						<div class="dir-item">
							<span class="dir-path">{dir}</span>
							<button class="dir-remove" onclick={() => removeProjectDir(i)} title="Remove">
								<X size={12} strokeWidth={2} />
							</button>
						</div>
					{/each}
				</div>
				<button class="browse-btn" onclick={openBrowser}>
					<FolderOpen size={13} strokeWidth={1.5} />
					Browse folder
				</button>
			{/if}
		{:else if activeTab === "guide"}
			<div class="guide" role="region" aria-label="Guide">
				<div class="guide-tab-bar">
					<button
						class="guide-tab"
						class:active={guideTab === "shortcuts"}
						onmousedown={(e) => e.preventDefault()}
						onclick={() => { guideTab = "shortcuts"; }}
						tabindex={-1}
					>Shortcuts</button>
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

				{#if guideTab === "shortcuts"}
					<div class="guide-table">
						<div class="guide-row">
							<kbd>{generalConfig.hotkey}</kbd>
							<span>Toggle Lychi</span>
						</div>
						<div class="guide-row">
							<kbd>Escape</kbd>
							<span>Hide / dismiss</span>
						</div>
						<div class="guide-row">
							<kbd>Enter</kbd>
							<span>Execute command</span>
						</div>
						<div class="guide-row">
							<kbd>Tab</kbd>
							<span>Accept completion</span>
						</div>
						<div class="guide-row">
							<kbd>Up / Down</kbd>
							<span>Navigate completions</span>
						</div>
						<div class="guide-row">
							<kbd>Ctrl+1</kbd>
							<span>Toggle history</span>
						</div>
						<div class="guide-row">
							<kbd>Ctrl+2</kbd>
							<span>Toggle media</span>
						</div>
						<div class="guide-row">
							<kbd>Ctrl+3</kbd>
							<span>Toggle settings</span>
						</div>
						<div class="guide-row">
							<kbd>Ctrl+4</kbd>
							<span>Toggle notes</span>
						</div>
						<div class="guide-row">
							<kbd>@</kbd>
							<span>File/folder reference</span>
						</div>
					</div>
				{:else if guideTab === "commands"}
					<div class="guide-table">
						<div class="guide-row">
							<code>open &lt;app&gt;</code>
							<span>Launch an app</span>
						</div>
						<div class="guide-row">
							<code>web &lt;query&gt;</code>
							<span>Search the web</span>
						</div>
						<div class="guide-row">
							<code>yt &lt;query&gt;</code>
							<span>Search YouTube</span>
						</div>
						<div class="guide-row">
							<code>run &lt;cmd&gt;</code>
							<span>Run a shell command</span>
						</div>
						<div class="guide-row">
							<code>calc &lt;expr&gt;</code>
							<span>Evaluate math</span>
						</div>
						<div class="guide-row">
							<code>file &lt;path&gt;</code>
							<span>Open file or folder</span>
						</div>
						<div class="guide-row">
							<code>project &lt;name&gt;</code>
							<span>Open project in editor</span>
						</div>
						<div class="guide-row">
							<code>spotify &lt;action&gt;</code>
							<span>Control Spotify</span>
						</div>
						<div class="guide-row">
							<code>media &lt;action&gt;</code>
							<span>Control any media player</span>
						</div>
						<div class="guide-row">
							<code>note &lt;text&gt;</code>
							<span>Save a quick note</span>
						</div>
						<div class="guide-row">
							<code>todo &lt;action&gt;</code>
							<span>Manage todo list</span>
						</div>
						<div class="guide-row">
							<code>system &lt;action&gt;</code>
							<span>shutdown / reboot / lock / suspend</span>
						</div>
					</div>
				{:else}
					<div class="guide-table">
						<div class="guide-row">
							<code>=2+2</code>
							<span>Calculator</span>
						</div>
						<div class="guide-row">
							<code>&gt;ls -la</code>
							<span>Shell command</span>
						</div>
						<div class="guide-row">
							<code>~/Downloads</code>
							<span>Open path</span>
						</div>
						<div class="guide-row">
							<code>github.com</code>
							<span>Open URL</span>
						</div>
					</div>
				{/if}
			</div>
		{:else if activeTab === "about"}
			<div class="about">
				<div class="about-header">
					<span class="about-name">Lychi</span>
					<span class="about-version">v{appVersion}</span>
				</div>
				<p class="about-desc">A fast, local-first command surface. Your data stays on your device. AI is optional, never required. Built for speed, privacy, and security.</p>

				<div class="about-links">
					<div class="about-link-row">
						<span class="about-link-label">Website</span>
						<span class="about-link-value">lychi.app</span>
					</div>
					<div class="about-link-row">
						<span class="about-link-label">Support</span>
						<span class="about-link-value">support@lychi.app</span>
					</div>
					<div class="about-link-row">
						<span class="about-link-label">Features</span>
						<span class="about-link-value">feat@lychi.app</span>
					</div>
				</div>

				<div class="about-credits">
					<span class="about-credits-title">Credits</span>
					<div class="about-links">
						<div class="about-link-row">
							<span class="about-link-label">Weather data</span>
							<span class="about-link-value">MET Norway (CC BY 4.0)</span>
						</div>
						<div class="about-link-row">
							<span class="about-link-label">Geocoding</span>
							<span class="about-link-value">OpenStreetMap contributors (ODbL)</span>
						</div>
						<div class="about-link-row">
							<span class="about-link-label">Geolocation</span>
							<span class="about-link-value">freeipapi.com</span>
						</div>
						<div class="about-link-row">
							<span class="about-link-label">Icons</span>
							<span class="about-link-value">Lucide (ISC)</span>
						</div>
					</div>
				</div>

				<p class="about-copy">&copy; {new Date().getFullYear()} Lychi. All rights reserved.</p>
			</div>
		{/if}

		{#if saveError}
			<div class="field-error">{saveError}</div>
		{/if}
	</div>
</div>

<style>
	.settings-panel {
		display: flex;
		max-height: 50vh;
		font-family: var(--font-mono);
		font-size: 13px;
		color: var(--fg);
	}

	.sidebar {
		display: flex;
		flex-direction: column;
		gap: 2px;
		padding: 12px 8px;
		border-right: 1px solid var(--border);
		flex-shrink: 0;
		width: 110px;
	}

	.tab-btn {
		display: flex;
		align-items: center;
		gap: 8px;
		background: none;
		border: none;
		color: var(--fg-muted);
		font-family: var(--font-mono);
		font-size: 11px;
		padding: 6px 10px;
		border-radius: 4px;
		cursor: pointer;
		transition: color 100ms ease, background 100ms ease;
		white-space: nowrap;
	}

	.tab-btn:hover {
		color: var(--fg);
		background: var(--bg-secondary);
	}

	.tab-btn.active {
		color: var(--fg);
		background: var(--bg-secondary);
	}

	.content {
		flex: 1;
		min-width: 0;
		padding: 12px 16px;
	}

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
		min-width: 70px;
	}

	input[type="password"],
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

	input[type="password"]:focus,
	input[type="text"]:focus {
		border-color: var(--fg-muted);
	}

	.key-row {
		display: flex;
		gap: 6px;
		flex: 1;
		min-width: 0;
	}

	.key-row input {
		flex: 1;
		min-width: 0;
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

	.hotkey-row {
		display: flex;
		gap: 6px;
		align-items: center;
	}

	.hotkey-btn {
		background: var(--bg-secondary);
		color: var(--fg);
		border: 1px solid var(--border);
		border-radius: 4px;
		padding: 5px 12px;
		font-family: var(--font-mono);
		font-size: 12px;
		min-width: 140px;
		text-align: center;
		cursor: pointer;
		transition: border-color 100ms ease;
	}

	.hotkey-btn:hover {
		border-color: var(--fg-muted);
	}

	.hotkey-btn.recording {
		border-color: var(--accent);
		color: var(--accent);
		animation: hotkey-pulse 1s ease-in-out infinite;
	}

	@keyframes hotkey-pulse {
		0%, 100% { opacity: 1; }
		50% { opacity: 0.4; }
	}

	.theme-toggle {
		display: flex;
		border: 1px solid var(--border);
		border-radius: 4px;
		overflow: hidden;
	}

	.theme-option {
		background: var(--bg-secondary);
		color: var(--fg-muted);
		border: none;
		padding: 5px 14px;
		font-family: var(--font-mono);
		font-size: 12px;
		cursor: pointer;
		transition: background 100ms ease, color 100ms ease;
		display: flex;
		align-items: center;
		justify-content: center;
	}

	.theme-option:first-child {
		border-right: 1px solid var(--border);
	}

	.theme-option:hover:not(.active) {
		color: var(--fg);
	}

	.theme-option.active {
		background: var(--border);
		color: var(--fg);
	}

	.checkbox {
		width: 18px;
		height: 18px;
		display: flex;
		align-items: center;
		justify-content: center;
		background: var(--bg-secondary);
		border: 1px solid var(--border);
		border-radius: 3px;
		color: var(--accent);
		cursor: pointer;
		padding: 0;
		flex-shrink: 0;
		transition: border-color 100ms ease;
	}

	.checkbox:hover {
		border-color: var(--fg-muted);
	}

	.checkbox.checked {
		background: var(--bg-secondary);
		border-color: var(--fg-muted);
	}

	.field-error {
		font-size: 11px;
		color: var(--error);
		padding: 2px 0 4px 0;
	}

	/* Projects tab */
	.dir-list {
		display: flex;
		flex-direction: column;
		gap: 2px;
		margin-bottom: 10px;
	}

	.dir-item {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 8px;
		padding: 5px 8px;
		background: var(--bg-secondary);
		border-radius: 4px;
	}

	.dir-path {
		font-size: 12px;
		color: var(--fg);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		min-width: 0;
	}

	.dir-remove {
		display: flex;
		align-items: center;
		justify-content: center;
		background: none;
		border: none;
		color: var(--fg-muted);
		cursor: pointer;
		padding: 2px;
		border-radius: 3px;
		flex-shrink: 0;
		transition: color 100ms ease;
	}

	.dir-remove:hover {
		color: var(--error);
	}

	.browse-btn {
		display: flex;
		align-items: center;
		gap: 6px;
		background: var(--bg);
		color: var(--fg-muted);
		border: 1px dashed var(--border);
		border-radius: 4px;
		padding: 6px 12px;
		font-family: var(--font-mono);
		font-size: 12px;
		cursor: pointer;
		width: 100%;
		justify-content: center;
	}

	.browse-btn:hover {
		color: var(--fg);
		border-color: var(--fg-muted);
	}

	/* Folder browser */
	.browser-header {
		display: flex;
		align-items: center;
		gap: 6px;
		margin-bottom: 6px;
	}

	.browser-input {
		flex: 1;
		min-width: 0;
		font-size: 11px;
		font-family: var(--font-mono);
		color: var(--fg);
		background: var(--bg-secondary);
		border: 1px solid var(--border);
		border-radius: 4px;
		padding: 3px 6px;
		outline: none;
	}

	.browser-input:focus {
		border-color: var(--fg-muted);
	}

	.browser-up {
		background: var(--bg-secondary);
		color: var(--fg-muted);
		border: 1px solid var(--border);
		border-radius: 4px;
		padding: 3px 8px;
		font-family: var(--font-mono);
		font-size: 11px;
		cursor: pointer;
		flex-shrink: 0;
	}

	.browser-up:hover:not(:disabled) {
		color: var(--fg);
		border-color: var(--fg-muted);
	}

	.browser-up:disabled {
		opacity: 0.3;
		cursor: default;
	}

	.browser-select,
	.browser-close {
		display: flex;
		align-items: center;
		justify-content: center;
		background: none;
		border: 1px solid var(--border);
		border-radius: 4px;
		padding: 3px 6px;
		cursor: pointer;
		flex-shrink: 0;
	}

	.browser-select {
		color: var(--accent);
	}

	.browser-select:hover {
		border-color: var(--accent);
	}

	.browser-close {
		color: var(--fg-muted);
	}

	.browser-close:hover {
		color: var(--error);
		border-color: var(--error);
	}

	.browser-list {
		display: flex;
		flex-direction: column;
		gap: 1px;
		max-height: 200px;
		overflow-y: auto;
	}

	.browser-item {
		display: flex;
		align-items: center;
		gap: 6px;
		padding: 4px 8px;
		background: none;
		border: none;
		color: var(--fg);
		font-family: var(--font-mono);
		font-size: 12px;
		cursor: pointer;
		border-radius: 3px;
		text-align: left;
	}

	.browser-item:hover {
		background: var(--bg-secondary);
	}

	.browser-item span {
		flex: 1;
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.browser-empty {
		font-size: 11px;
		color: var(--fg-muted);
		padding: 8px;
		text-align: center;
	}

	/* Guide tab */
	.guide {
		display: flex;
		flex-direction: column;
		padding: 2px 0;
		overflow-y: auto;
		max-height: calc(50vh - 40px);
	}

	.guide-tab-bar {
		display: flex;
		border-bottom: 1px solid var(--border);
		margin-bottom: 8px;
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

	.guide-row kbd,
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

	/* About tab */
	.about-tab {
		margin-top: 8px;
		border-top: 1px solid var(--border);
		padding-top: 8px;
	}

	.about {
		display: flex;
		flex-direction: column;
		gap: 12px;
		padding: 4px 0;
	}

	.about-header {
		display: flex;
		align-items: baseline;
		gap: 8px;
	}

	.about-name {
		font-size: 16px;
		font-weight: 600;
		color: var(--fg);
	}

	.about-version {
		font-size: 12px;
		color: var(--fg-muted);
	}

	.about-desc {
		font-size: 12px;
		color: var(--fg-muted);
		line-height: 1.5;
		margin: 0;
	}

	.about-links {
		display: flex;
		flex-direction: column;
		gap: 4px;
		margin-top: 4px;
	}

	.about-link-row {
		display: flex;
		align-items: center;
		justify-content: space-between;
		padding: 5px 8px;
		background: var(--bg-secondary);
		border-radius: 4px;
	}

	.about-link-label {
		font-size: 12px;
		color: var(--fg-muted);
	}

	.about-link-value {
		font-size: 12px;
		color: var(--fg);
	}

	.about-credits {
		display: flex;
		flex-direction: column;
		gap: 8px;
		padding-top: 4px;
		border-top: 1px solid var(--border);
	}

	.about-credits-title {
		font-size: 12px;
		font-weight: 600;
		color: var(--fg-muted);
		text-transform: uppercase;
		letter-spacing: 0.5px;
	}

	.about-copy {
		font-size: 11px;
		color: var(--fg-muted);
		margin: 4px 0 0 0;
		opacity: 0.6;
	}
</style>

<script lang="ts">
import {
	BookOpen,
	Check,
	ChevronRight,
	FolderOpen,
	Info,
	Keyboard,
	Moon,
	RotateCcw,
	SlidersHorizontal,
	Sparkles,
	Sun,
	X,
} from "lucide-svelte";
import { onMount } from "svelte";
import type {
	AiConfig,
	CommandsConfig,
	DirEntry,
	GeneralConfig,
	KeybindingsConfig,
	PrivacyConfig,
	ProjectsConfig,
} from "$lib/ipc";
import {
	checkAiHealth,
	KEYBINDINGS_DEFAULTS,
	listDirectories,
	recordHotkey,
	restartApp,
	saveAiConfig,
	saveCommandsConfig,
	saveGeneralConfig,
	saveKeybindingsConfig,
	savePrivacyConfig,
	saveProjectsConfig,
	setApiKey,
	setHotkey,
} from "$lib/ipc";
import {
	ACTION_LABELS,
	type ActionId,
	ALL_ACTIONS,
	comboFromEvent,
	findConflicts,
	loadKeybindings,
} from "$lib/keybindings";
import { invalidateSettings, preloadSettings } from "$lib/preloadCache";
import Select from "./Select.svelte";

let { ondismiss }: { ondismiss: () => void } = $props();

let activeTab: "general" | "ai" | "projects" | "shortcuts" | "guide" | "about" = $state("general");
let guideTab: "commands" | "triggers" = $state("commands");
let appVersion = $state("");
let layerShellSupported = $state(false);
let activeWindowStrategy = $state("auto");

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
	monitor_mode: "cursor",
	window_strategy: "auto",
});
let commandsConfig: CommandsConfig = $state({
	default_search_engine: "https://www.google.com/search?q=",
	youtube_url: "https://www.youtube.com/results?search_query=",
	shell: "/bin/bash",
});

let privacyConfig: PrivacyConfig = $state({
	allow_ip_geolocation: false,
	allow_public_ip: false,
});
let keybindingsConfig: KeybindingsConfig = $state({ ...KEYBINDINGS_DEFAULTS });
let recordingAction: ActionId | null = $state(null);
let conflictWarning = $state("");
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

onMount(() => {
	// Non-blocking: render with defaults, update when data arrives in next frame
	preloadSettings().then((cached) => {
		requestAnimationFrame(() => {
			aiConfig = cached.aiConfig;
			generalConfig = cached.generalConfig;
			commandsConfig = cached.commandsConfig;
			privacyConfig = cached.privacyConfig;
			keybindingsConfig = cached.keybindingsConfig;
			customShell = !knownShells.includes(cached.commandsConfig.shell);
			projectDirs = cached.projectsConfig.directories;
			appVersion = cached.appVersion;
			layerShellSupported = cached.layerShellSupported;
			activeWindowStrategy = cached.activeWindowStrategy;

			// These depend on cached data, fire after settings arrive
			fetchModels(cached.aiConfig.mode).then((m) => {
				providerModels = m;
			});
			refreshHealth();
		});
	});
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

// --- Keybindings ---

function startRecording(action: ActionId) {
	recordingAction = action;
	conflictWarning = "";

	const handler = (e: KeyboardEvent) => {
		e.preventDefault();
		e.stopPropagation();

		const combo = comboFromEvent(e);
		if (!combo) return; // bare modifier key, wait for real key

		// Escape without modifiers = cancel
		if (e.key === "Escape" && !e.ctrlKey && !e.shiftKey && !e.altKey) {
			recordingAction = null;
			window.removeEventListener("keydown", handler, true);
			return;
		}

		// Check for conflicts
		const testConfig = { ...keybindingsConfig, [action]: combo };
		const conflicts = findConflicts(testConfig);
		if (conflicts.length > 0) {
			const [a, b] = conflicts[0];
			const other = a === action ? b : a;
			conflictWarning = `"${combo}" conflicts with ${ACTION_LABELS[other]}`;
		} else {
			conflictWarning = "";
		}

		// Apply
		keybindingsConfig = { ...keybindingsConfig, [action]: combo };
		loadKeybindings(keybindingsConfig);
		saveKeybindingsConfig(keybindingsConfig);
		invalidateSettings();
		recordingAction = null;
		window.removeEventListener("keydown", handler, true);
	};

	window.addEventListener("keydown", handler, true);
}

async function resetAllShortcuts() {
	keybindingsConfig = { ...KEYBINDINGS_DEFAULTS };
	loadKeybindings(keybindingsConfig);
	await saveKeybindingsConfig(keybindingsConfig);
	invalidateSettings();
	conflictWarning = "";
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
	// C6: Fetch remote models now that AI is enabled (was skipped if mode was disabled)
	if (val !== "disabled") {
		cachedManifest = null;
		providerModels = await fetchModels(val);
	}
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

async function fetchModels(aiMode: string): Promise<ModelManifest> {
	if (cachedManifest) return cachedManifest;
	// C6: Only fetch remote models when AI is actually enabled.
	// No network calls in default (disabled) mode.
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

async function handleMonitorModeChange(val: string) {
	generalConfig.monitor_mode = val;
	try {
		await saveGeneralConfig(generalConfig);
	} catch (err) {
		console.error("[settings] Failed to save monitor mode:", err);
		saveError = `Failed to save: ${err}`;
	}
}

// Resolve what strategy the config would pick at next startup
function resolveStrategy(strategy: string): string {
	if (strategy === "layer-shell") return layerShellSupported ? "layer-shell" : "x11";
	if (strategy === "x11") return "x11";
	return layerShellSupported ? "layer-shell" : "x11"; // "auto"
}

let strategyNeedsRestart = $derived(
	resolveStrategy(generalConfig.window_strategy) !== activeWindowStrategy,
);

async function handleWindowStrategyChange(val: string) {
	generalConfig.window_strategy = val;
	try {
		await saveGeneralConfig(generalConfig);
	} catch (err) {
		console.error("[settings] Failed to save window strategy:", err);
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
			class:active={activeTab === "shortcuts"}
			onclick={() => (activeTab = "shortcuts")}
		>
			<Keyboard size={14} strokeWidth={1.5} />
			<span>Shortcuts</span>
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
				<label for="monitor-mode">Open on</label>
				<Select
					id="monitor-mode"
					value={generalConfig.monitor_mode}
					options={[
						{ value: "cursor", label: "Current monitor" },
						{ value: "primary", label: "Primary monitor" },
					]}
					onchange={handleMonitorModeChange}
				/>
			</div>
			<div class="field">
				<label for="window-strategy">Window strategy</label>
				<Select
					id="window-strategy"
					value={generalConfig.window_strategy}
					options={[
						{ value: "auto", label: "Auto (recommended)" },
						...(layerShellSupported
							? [{ value: "layer-shell", label: "Layer shell (Wayland)" }]
							: [{ value: "x11", label: "X11 positioned" }]),
					]}
					onchange={handleWindowStrategyChange}
				/>
				{#if strategyNeedsRestart}
					<button class="restart-btn" onclick={() => restartApp()}>
						Restart to apply
					</button>
				{/if}
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
			<div class="section-label">Privacy</div>
			<div class="field">
				<label for="allow-geolocation">Allow IP geolocation</label>
				<button
					id="allow-geolocation"
					class="checkbox"
					class:checked={privacyConfig.allow_ip_geolocation}
					onclick={async () => {
						privacyConfig.allow_ip_geolocation = !privacyConfig.allow_ip_geolocation;
						await savePrivacyConfig(privacyConfig);
					}}
					role="checkbox"
					aria-checked={privacyConfig.allow_ip_geolocation}
				>
					{#if privacyConfig.allow_ip_geolocation}
						<svg width="12" height="12" viewBox="0 0 12 12" fill="none">
							<path d="M2 6L5 9L10 3" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"/>
						</svg>
					{/if}
				</button>
			</div>
			<div class="field-hint">Weather auto-detect via freeipapi.com</div>
			<div class="field">
				<label for="allow-public-ip">Allow public IP lookup</label>
				<button
					id="allow-public-ip"
					class="checkbox"
					class:checked={privacyConfig.allow_public_ip}
					onclick={async () => {
						privacyConfig.allow_public_ip = !privacyConfig.allow_public_ip;
						await savePrivacyConfig(privacyConfig);
					}}
					role="checkbox"
					aria-checked={privacyConfig.allow_public_ip}
				>
					{#if privacyConfig.allow_public_ip}
						<svg width="12" height="12" viewBox="0 0 12 12" fill="none">
							<path d="M2 6L5 9L10 3" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"/>
						</svg>
					{/if}
				</button>
			</div>
			<div class="field-hint">sysinfo net/ip via ifconfig.me</div>
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
		{:else if activeTab === "shortcuts"}
			<div class="section-label">Keyboard Shortcuts</div>
			{#each ALL_ACTIONS as action}
				<div class="field shortcut-row">
					<span class="field-label">{ACTION_LABELS[action]}</span>
					<button
						class="hotkey-btn"
						class:recording={recordingAction === action}
						onclick={() => startRecording(action)}
					>
						{#if recordingAction === action}
							Press keys...
						{:else}
							{keybindingsConfig[action]}
						{/if}
					</button>
				</div>
			{/each}
			{#if conflictWarning}
				<div class="field-error">{conflictWarning}</div>
			{/if}
			<div class="field" style="justify-content: flex-end; padding-top: 8px;">
				<button class="reset-btn" onclick={resetAllShortcuts}>
					<RotateCcw size={12} strokeWidth={1.5} />
					Reset all to defaults
				</button>
			</div>
		{:else if activeTab === "guide"}
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
		width: 120px;
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
		width: 120px;
	}


	.section-label {
		font-size: 11px;
		color: var(--fg-muted);
		text-transform: uppercase;
		letter-spacing: 0.05em;
		padding: 12px 0 4px;
		border-top: 1px solid var(--border);
		margin-top: 4px;
	}

	.field-hint {
		font-size: 10px;
		color: var(--fg-muted);
		opacity: 0.7;
		padding: 0 0 2px;
	}

	.restart-btn {
		font-size: 11px;
		padding: 2px 8px;
		border-radius: 4px;
		border: 1px solid var(--warning, #f59e0b);
		background: transparent;
		color: var(--warning, #f59e0b);
		cursor: pointer;
		flex-shrink: 0;
	}

	.restart-btn:hover {
		background: var(--warning, #f59e0b);
		color: var(--bg);
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

	/* Shortcuts tab */
	.shortcut-row {
		display: flex;
		align-items: center;
		justify-content: space-between;
	}

	.shortcut-row .hotkey-btn {
		min-width: 110px;
	}

	.reset-btn {
		display: flex;
		align-items: center;
		gap: 5px;
		background: var(--bg-secondary);
		color: var(--fg-muted);
		border: 1px solid var(--border);
		border-radius: 4px;
		padding: 4px 10px;
		font-size: 11px;
		cursor: pointer;
		transition: color 100ms ease, border-color 100ms ease;
	}

	.reset-btn:hover {
		color: var(--fg);
		border-color: var(--fg-muted);
	}
</style>

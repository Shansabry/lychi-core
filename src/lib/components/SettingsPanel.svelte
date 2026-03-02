<script lang="ts">
import { BookOpen, FolderOpen, Info, Keyboard, SlidersHorizontal, Sparkles } from "lucide-svelte";
import { onMount } from "svelte";
import type {
	AiConfig,
	CommandsConfig,
	GeneralConfig,
	KeybindingsConfig,
	PrivacyConfig,
} from "$lib/ipc";
import { KEYBINDINGS_DEFAULTS } from "$lib/ipc";
import { preloadSettings } from "$lib/preloadCache";
import AboutTab from "./settings/AboutTab.svelte";
import AiTab from "./settings/AiTab.svelte";
import GeneralTab from "./settings/GeneralTab.svelte";
import GuideTab from "./settings/GuideTab.svelte";
import ProjectsTab from "./settings/ProjectsTab.svelte";
import ShortcutsTab from "./settings/ShortcutsTab.svelte";

let { ondismiss }: { ondismiss: () => void } = $props();

let activeTab: "general" | "ai" | "projects" | "shortcuts" | "guide" | "about" = $state("general");
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
	terminal: "",
});
let privacyConfig: PrivacyConfig = $state({
	allow_ip_geolocation: false,
	allow_public_ip: false,
});
let keybindingsConfig: KeybindingsConfig = $state({ ...KEYBINDINGS_DEFAULTS });
let projectDirs: string[] = $state([]);

let saveError = $state("");

let aiTabRef: AiTab | undefined = $state();
let generalTabRef: GeneralTab | undefined = $state();

onMount(() => {
	preloadSettings().then((cached) => {
		requestAnimationFrame(() => {
			aiConfig = cached.aiConfig;
			generalConfig = cached.generalConfig;
			commandsConfig = cached.commandsConfig;
			privacyConfig = cached.privacyConfig;
			keybindingsConfig = cached.keybindingsConfig;
			projectDirs = cached.projectsConfig.directories;
			appVersion = cached.appVersion;
			layerShellSupported = cached.layerShellSupported;
			activeWindowStrategy = cached.activeWindowStrategy;

			generalTabRef?.initCustomShell(cached.commandsConfig.shell);
			aiTabRef?.initModels(cached.aiConfig.mode);
		});
	});
});

function handleKeydown(e: KeyboardEvent) {
	if (e.key === "Escape") {
		if (aiTabRef?.dismissConfirm()) {
			e.preventDefault();
			e.stopPropagation();
			return;
		}
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
			<GeneralTab
				bind:this={generalTabRef}
				bind:generalConfig
				bind:commandsConfig
				bind:privacyConfig
				{layerShellSupported}
				{activeWindowStrategy}
				onsaveerror={(msg) => (saveError = msg)}
			/>
		{:else if activeTab === "ai"}
			<AiTab
				bind:this={aiTabRef}
				bind:aiConfig
				onsaveerror={(msg) => (saveError = msg)}
			/>
		{:else if activeTab === "projects"}
			<ProjectsTab
				bind:projectDirs
				onsaveerror={(msg) => (saveError = msg)}
			/>
		{:else if activeTab === "shortcuts"}
			<ShortcutsTab
				bind:keybindingsConfig
				onsaveerror={(msg) => (saveError = msg)}
			/>
		{:else if activeTab === "guide"}
			<GuideTab />
		{:else if activeTab === "about"}
			<AboutTab {appVersion} />
		{/if}

		{#if saveError}
			<div class="field-error">{saveError}</div>
		{/if}
	</div>
</div>

<style>
	.settings-panel {
		display: flex;
		max-height: 65vh;
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
		overflow-y: auto;
	}

	.about-tab {
		margin-top: 8px;
		border-top: 1px solid var(--border);
		padding-top: 8px;
	}

	.field-error {
		font-size: 11px;
		color: var(--error);
		padding: 2px 0 4px 0;
	}
</style>

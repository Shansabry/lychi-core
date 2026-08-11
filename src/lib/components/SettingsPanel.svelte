<script lang="ts">
import {
	Archive,
	BookOpen,
	CircleCheck,
	FolderOpen,
	Info,
	Keyboard,
	SlidersHorizontal,
	Sparkles,
} from "lucide-svelte";
import { onMount } from "svelte";
import type {
	AiConfig,
	CommandsConfig,
	GeneralConfig,
	KeybindingsConfig,
	PrivacyConfig,
} from "$lib/ipc";
import { KEYBINDINGS_DEFAULTS, PRIVACY_DEFAULTS } from "$lib/ipc";
import { preloadSettings } from "$lib/preloadCache";
import AboutTab from "./settings/AboutTab.svelte";
import AiTab from "./settings/ai/AiTab.svelte";
import DataTab from "./settings/DataTab.svelte";
import GeneralTab from "./settings/GeneralTab.svelte";
import GuideTab from "./settings/GuideTab.svelte";
import ProjectsTab from "./settings/ProjectsTab.svelte";
import SetupTab from "./settings/SetupTab.svelte";
import ShortcutsTab from "./settings/ShortcutsTab.svelte";

type Tab = "general" | "ai" | "projects" | "shortcuts" | "data" | "guide" | "setup" | "about";

let { ondismiss, onpresetchange }: { ondismiss: () => void; onpresetchange?: () => void } =
	$props();

let activeTab: Tab = $state("general");

/// Jump to a tab. Exported rather than taken as an `initialTab` prop: this
/// panel is always mounted (hidden via CSS, per the first-render cost pattern),
/// so a prop read at init would capture whatever the value was at startup and
/// never see a later change. A `$derived` prop would be worse still — it would
/// yank the user back whenever anything upstream re-rendered.
export function showTab(tab: Tab) {
	activeTab = tab;
}
let appVersion = $state("");
let layerShellSupported = $state(false);
let activeWindowStrategy = $state("auto");
let screenComposited = $state(true);

let aiConfig: AiConfig = $state({
	mode: "disabled",
	provider: "anthropic",
	model: "",
	base_url: "",
	wire_format: "",
	ollama_url: "",
	ollama_model: "",
	local_model: "",
	// Placeholder shown only until `preloadSettings()` loads the real config in
	// onMount; these must MATCH the Rust defaults (config/schema.rs
	// default_ai_timeout / default_max_tokens) so a fresh user never briefly
	// sees a wrong value that could be saved. 300 here contradicted the real
	// 4096 max-token default — the streaming agent needs the headroom or long
	// answers truncate mid-sentence.
	timeout_secs: 8,
	max_tokens: 4096,
});
let generalConfig: GeneralConfig = $state({
	hide_on_blur: true,
	show_duration_ms: true,
	theme: "dark",
	accent: "",
	hotkey: "Super+Space",
	window_x: null,
	window_y: null,
	monitor_mode: "cursor",
	window_strategy: "auto",
	first_run_completed: false,
});
let commandsConfig: CommandsConfig = $state({
	default_search_engine: "https://www.google.com/search?q=",
	youtube_url: "https://www.youtube.com/results?search_query=",
	shell: "/bin/bash",
	terminal: "",
	extra_terminals: [],
	extra_ides: [],
	terminal_routing: "manual",
	search_engines: {},
});
// Protecting until the real config arrives, so a slow load never renders the
// clipboard toggle as off.
let privacyConfig: PrivacyConfig = $state(structuredClone(PRIVACY_DEFAULTS));
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
			screenComposited = cached.screenComposited;

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
			class="tab-btn"
			class:active={activeTab === "data"}
			onclick={() => (activeTab = "data")}
		>
			<Archive size={14} strokeWidth={1.5} />
			<span>Data</span>
		</button>
		<!-- Last, beside About: a diagnostics surface people visit occasionally,
		     not the reason they opened Settings. -->
		<button
			class="tab-btn setup-tab"
			class:active={activeTab === "setup"}
			onclick={() => (activeTab = "setup")}
		>
			<CircleCheck size={14} strokeWidth={1.5} />
			<span>Setup</span>
		</button>
		<button
			class="tab-btn"
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
				{screenComposited}
				onsaveerror={(msg) => (saveError = msg)}
			/>
		{:else if activeTab === "ai"}
			<AiTab
				bind:this={aiTabRef}
				bind:aiConfig
				onsaveerror={(msg) => (saveError = msg)}
				{onpresetchange}
			/>
		{:else if activeTab === "projects"}
			<ProjectsTab
				bind:projectDirs
				onsaveerror={(msg) => (saveError = msg)}
			/>
		{:else if activeTab === "data"}
			<DataTab onsaveerror={(msg) => (saveError = msg)} />
		{:else if activeTab === "shortcuts"}
			<ShortcutsTab
				bind:keybindingsConfig
				bind:commandsConfig
				onsaveerror={(msg) => (saveError = msg)}
			/>
		{:else if activeTab === "guide"}
			<GuideTab />
		{:else if activeTab === "setup"}
			<!-- Mounted only while shown: reading the checklist probes D-Bus and
			     may run the login shell, which must never happen at startup. -->
			<SetupTab onopentab={(tab) => showTab(tab as Tab)} />
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
		/* Fill the height the parent <main> allows (main is max-height:60vh with
		   overflow:hidden). A fixed vh here would exceed main and get clipped —
		   min-height:0 lets the flex children shrink so .content scrolls fully. */
		flex: 1;
		min-height: 0;
		max-height: 100%;
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
		min-height: 0;
		padding: 12px 16px;
		overflow-y: auto;
	}

	/* Divides the settings you change from the pages you consult. Sits on
	   whichever tab starts that trailing group — Setup today, About before it. */
	.setup-tab {
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

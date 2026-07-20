// Public IPC API for the frontend.
//
// Types are re-exported from the tauri-specta-generated `bindings.ts` so the
// Rust↔TS contract stays in sync automatically (no hand-written drift). Each
// function wraps the generated `commands.<name>` method and unwraps its Result,
// preserving the exact non-Tauri fallback the old hand-written layer had.

// Local imports for the generated types used in this file's function signatures.
// (A bare `export type { … } from` re-exports but does not bind the names locally.)
import type {
	AgentPlan,
	AiConfig,
	AiStatus,
	AllNotes,
	AllSettings,
	CommandsConfig,
	CompletionItem,
	CreditBalance,
	DirEntry,
	EnvironmentContext,
	FilePreviewData,
	FirebaseUser,
	GeneralConfig,
	HotkeyStatus,
	KeybindingsConfig,
	MountPoint,
	NoteItem,
	OllamaModelInfo,
	PrivacyConfig,
	ProjectsConfig,
	ReminderItem,
	SnippetItem,
	TimerStatus,
	TodoItem,
	TrackInfo,
} from "./bindings";
import { commands, type Result } from "./bindings";

// --- Re-exported generated types (single source of truth = Rust) ---
export type {
	AgentPlan,
	AgentStep,
	AiConfig,
	AiStatus,
	AliasItem,
	AllNotes,
	AllSettings,
	ClipboardContentType,
	CommandsConfig,
	CompletionItem,
	ContainerInfo,
	CreditBalance,
	DirChild,
	DirEntry,
	DockerContext,
	EnvironmentContext,
	FilePreviewData,
	FirebaseUser,
	GeneralConfig,
	GitContext,
	HotkeyStatus,
	KeybindingsConfig,
	MountPoint,
	NetworkContext,
	NoteItem,
	OllamaModelInfo,
	OutputType,
	PlaybackStatus,
	PrivacyConfig,
	ProjectContext,
	ProjectKind,
	ProjectScript,
	ProjectsConfig,
	ReminderItem,
	RiskLevel,
	SnippetItem,
	TimerStatus,
	TodoItem,
	TrackInfo,
	WindowContext,
} from "./bindings";

// `CommandResult` is the frontend name for the generated `CommandResultDto` —
// the flat wire shape the executor assembles from the internal sum-type
// `ActionResult` + its envelope. The public TS API keeps the historical name.
import type { CommandResultDto } from "./bindings";
export type CommandResult = CommandResultDto;

// --- Types that only exist in the hand-written layer (no bindings equivalent) ---

export interface FileSearchResult {
	label: string;
	full_path: string;
	is_dir: boolean;
	score: number;
	description?: string | null;
	size_bytes?: number | null;
	modified_secs?: number | null;
}

export interface FileSearchBatch {
	search_id: number;
	results: FileSearchResult[];
	done: boolean;
	has_ignore_rules?: boolean;
}

export interface StepEvent {
	plan_id: string;
	step_index: number;
	status: "running" | "done" | "failed";
	result?: CommandResult | null;
}

export interface BrowserContext {
	type: "GitHub" | "Localhost" | "StackOverflow" | "Documentation" | "Unknown";
	owner?: string;
	repo?: string;
	port?: number;
}

// --- Result unwrap helper ---

function unwrap<T>(r: Result<T, string>): T {
	if (r.status === "ok") return r.data;
	throw new Error(r.error);
}

function isTauri(): boolean {
	return "__TAURI_INTERNALS__" in window;
}

// --- Core command execution ---

export async function executeCommand(
	input: string,
	confirmed?: boolean,
	runInline?: boolean,
): Promise<CommandResult> {
	if (!isTauri()) {
		return {
			success: false,
			output: null,
			error: "Not running in Tauri",
			duration_ms: 0,
		} as CommandResult;
	}
	return unwrap(await commands.executeCommand(input, confirmed ?? null, runInline ?? null));
}

export async function getHistory(): Promise<string[]> {
	if (!isTauri()) return [];
	return unwrap(await commands.getHistory());
}

export async function clearHistory(): Promise<void> {
	if (!isTauri()) return;
	unwrap(await commands.clearHistory());
}

export async function hideWindow(): Promise<void> {
	if (!isTauri()) return;
	// Hide immediately — the compositor unmap is instantaneous, and delaying
	// the hide (an old 100ms close animation) created a window where a
	// toggle meant to REOPEN the launcher saw it still visible and hid it
	// again (the "press the hotkey twice" bug).
	await commands.hideLauncher();
}

export async function getHideOnBlur(): Promise<boolean> {
	if (!isTauri()) return true;
	return unwrap(await commands.getHideOnBlur());
}

export async function getCompletions(input: string): Promise<CompletionItem[]> {
	if (!isTauri()) return [];
	return unwrap(await commands.getCompletions(input));
}

export async function listPathCompletions(partial: string): Promise<CompletionItem[]> {
	if (!isTauri()) return [];
	return unwrap(await commands.listPathCompletions(partial));
}

export async function listDirectories(path: string): Promise<DirEntry[]> {
	if (!isTauri()) return [];
	return unwrap(await commands.listDirectories(path));
}

// --- Recursive file search ---

export async function getMountPoints(): Promise<MountPoint[]> {
	if (!isTauri()) return [];
	return unwrap(await commands.getMountPoints());
}

export async function startFileSearch(
	query: string,
	scope: string,
	searchId: number,
): Promise<void> {
	if (!isTauri()) return;
	unwrap(await commands.startFileSearch(query, scope, searchId));
}

export async function cancelFileSearch(): Promise<void> {
	if (!isTauri()) return;
	unwrap(await commands.cancelFileSearch());
}

export async function saveWindowPosition(x: number, y: number): Promise<void> {
	if (!isTauri()) return;
	unwrap(await commands.saveWindowPosition(x, y));
}

export async function openUri(uri: string): Promise<void> {
	if (!isTauri()) return;
	unwrap(await commands.openUri(uri));
}

/** Reveal a file/folder in the file manager, selected within its parent. */
export async function revealPath(path: string): Promise<void> {
	if (!isTauri()) return;
	unwrap(await commands.revealPath(path));
}

/** Open a path if it exists. Returns true if opened, false if it doesn't exist. */
export async function openPath(path: string): Promise<boolean> {
	if (!isTauri()) return false;
	return unwrap(await commands.openPath(path));
}

// --- Batch settings (single IPC call) ---

export async function getAllSettings(): Promise<AllSettings> {
	if (!isTauri())
		return {
			ai: {
				mode: "disabled",
				provider: "anthropic",
				model: "",
				ollama_url: "",
				ollama_model: "",
				timeout_secs: 8,
				max_tokens: 300,
			},
			general: {
				hide_on_blur: true,
				show_duration_ms: true,
				theme: "dark",
				hotkey: "Ctrl+Space",
				window_x: null,
				window_y: null,
				monitor_mode: "cursor",
				window_strategy: "auto",
				first_run_completed: false,
			},
			commands: {
				default_search_engine: "https://www.google.com/search?q=",
				youtube_url: "https://www.youtube.com/results?search_query=",
				shell: "/bin/bash",
				terminal: "",
				terminal_routing: "manual",
				search_engines: {},
			},
			projects: { directories: [] },
			privacy: { allow_ip_geolocation: false, allow_public_ip: false },
			keybindings: {
				toggle_history: "Ctrl+1",
				toggle_notes: "Ctrl+2",
				toggle_media: "Ctrl+3",
				toggle_settings: "Ctrl+4",
				open_inline_url: "Ctrl+O",
				submit: "Enter",
				dismiss: "Escape",
				tab_complete: "Tab",
				tab_back: "Shift+Tab",
				switch_scope: "Ctrl+Tab",
				web_search: "Ctrl+Enter",
				run_inline: "Shift+Enter",
				copy_path: "Ctrl+Shift+C",
				screenshot: "Ctrl+Shift+S",
			},
			app_version: "0.0.0",
			layer_shell_supported: false,
			active_window_strategy: "x11",
			screen_composited: true,
		};
	return unwrap(await commands.getAllSettings());
}

export async function getAllNotes(): Promise<AllNotes> {
	if (!isTauri()) return { notes: [], todos: [] };
	return unwrap(await commands.getAllNotes());
}

// --- General Config ---

export async function getGeneralConfig(): Promise<GeneralConfig> {
	if (!isTauri())
		return {
			hide_on_blur: true,
			show_duration_ms: true,
			theme: "dark",
			hotkey: "Ctrl+Space",
			window_x: null,
			window_y: null,
			monitor_mode: "cursor",
			window_strategy: "auto",
			first_run_completed: false,
		};
	return unwrap(await commands.getGeneralConfig());
}

export async function saveGeneralConfig(general: GeneralConfig): Promise<void> {
	if (!isTauri()) return;
	unwrap(await commands.saveGeneralConfig(general));
}

export async function getLayerShellSupported(): Promise<boolean> {
	if (!isTauri()) return false;
	return commands.getLayerShellSupported();
}

export async function getActiveWindowStrategy(): Promise<string> {
	if (!isTauri()) return "x11";
	return commands.getActiveWindowStrategy();
}

export async function getHotkeyStatus(): Promise<HotkeyStatus> {
	if (!isTauri()) return { registered: false, session_type: "x11", desktop: "", reliable: false };
	return commands.getHotkeyStatus();
}

export async function getAutostartEnabled(): Promise<boolean> {
	if (!isTauri()) return false;
	return unwrap(await commands.getAutostartEnabled());
}

export async function setAutostartEnabled(enabled: boolean): Promise<void> {
	if (!isTauri()) return;
	unwrap(await commands.setAutostartEnabled(enabled));
}

// --- Commands Config ---

export async function getCommandsConfig(): Promise<CommandsConfig> {
	if (!isTauri())
		return {
			default_search_engine: "https://www.google.com/search?q=",
			youtube_url: "https://www.youtube.com/results?search_query=",
			shell: "/bin/bash",
			terminal: "",
			terminal_routing: "manual",
			search_engines: {},
		};
	return unwrap(await commands.getCommandsConfig());
}

export async function getInstalledTerminals(): Promise<string[]> {
	if (!isTauri()) return ["xterm"];
	return commands.getInstalledTerminals();
}

export async function saveCommandsConfig(commandsConfig: CommandsConfig): Promise<void> {
	if (!isTauri()) return;
	unwrap(await commands.saveCommandsConfig(commandsConfig));
}

// --- Projects Config ---

export async function getProjectsConfig(): Promise<ProjectsConfig> {
	if (!isTauri())
		return {
			directories: ["~/Projects", "~/Dev", "~/Code", "~/repos"],
		};
	return unwrap(await commands.getProjectsConfig());
}

export async function saveProjectsConfig(projects: ProjectsConfig): Promise<void> {
	if (!isTauri()) return;
	unwrap(await commands.saveProjectsConfig(projects));
}

// --- Privacy ---

export async function getPrivacyConfig(): Promise<PrivacyConfig> {
	if (!isTauri()) return { allow_ip_geolocation: false, allow_public_ip: false };
	return unwrap(await commands.getPrivacyConfig());
}

export async function savePrivacyConfig(privacy: PrivacyConfig): Promise<void> {
	if (!isTauri()) return;
	unwrap(await commands.savePrivacyConfig(privacy));
}

/** C6: Grant a specific privacy consent and persist to config. */
export async function grantPrivacyConsent(feature: string): Promise<void> {
	if (!isTauri()) return;
	unwrap(await commands.grantPrivacyConsent(feature));
}

export async function restartApp(): Promise<void> {
	if (!isTauri()) return;
	return commands.restartApp();
}

export async function setHotkey(hotkey: string): Promise<void> {
	if (!isTauri()) return;
	unwrap(await commands.setHotkey(hotkey));
}

export async function recordHotkey(): Promise<string> {
	if (!isTauri()) return "";
	return unwrap(await commands.recordHotkey());
}

// --- Keybindings ---

export const KEYBINDINGS_DEFAULTS: KeybindingsConfig = {
	toggle_history: "Ctrl+1",
	toggle_notes: "Ctrl+2",
	toggle_media: "Ctrl+3",
	toggle_settings: "Ctrl+4",
	open_inline_url: "Ctrl+O",
	submit: "Enter",
	dismiss: "Escape",
	tab_complete: "Tab",
	tab_back: "Shift+Tab",
	switch_scope: "Ctrl+Tab",
	web_search: "Ctrl+Enter",
	run_inline: "Shift+Enter",
	copy_path: "Ctrl+Shift+C",
	screenshot: "Ctrl+Shift+S",
};

export async function getKeybindingsConfig(): Promise<KeybindingsConfig> {
	if (!isTauri()) return { ...KEYBINDINGS_DEFAULTS };
	return unwrap(await commands.getKeybindingsConfig());
}

export async function saveKeybindingsConfig(keybindings: KeybindingsConfig): Promise<void> {
	if (!isTauri()) return;
	unwrap(await commands.saveKeybindingsConfig(keybindings));
}

// --- AI ---

export async function getAiConfig(): Promise<AiConfig> {
	if (!isTauri())
		return {
			mode: "disabled",
			provider: "anthropic",
			model: "",
			ollama_url: "",
			ollama_model: "",
			timeout_secs: 8,
			max_tokens: 300,
		};
	return unwrap(await commands.getAiConfig());
}

export async function saveAiConfig(aiConfig: AiConfig): Promise<void> {
	if (!isTauri()) return;
	unwrap(await commands.saveAiConfig(aiConfig));
}

export async function setApiKey(provider: string, key: string): Promise<void> {
	if (!isTauri()) return;
	unwrap(await commands.setApiKey(provider, key));
}

export async function getMaskedApiKey(provider: string): Promise<string | null> {
	if (!isTauri()) return null;
	return unwrap(await commands.getMaskedApiKey(provider));
}

export async function getAiStatus(): Promise<AiStatus> {
	if (!isTauri()) return { mode: "disabled", provider: "", model: "", has_ai_router: false };
	return unwrap(await commands.getAiStatus());
}

export async function checkAiHealth(): Promise<boolean> {
	if (!isTauri()) return false;
	return unwrap(await commands.checkAiHealth());
}

export async function listOllamaModels(): Promise<OllamaModelInfo[]> {
	if (!isTauri()) return [];
	return unwrap(await commands.listOllamaModels());
}

// --- Firebase / Cloud ---

export async function firebaseSignIn(): Promise<void> {
	if (!isTauri()) return;
	unwrap(await commands.firebaseSignIn());
}

export async function firebaseSignOut(): Promise<void> {
	if (!isTauri()) return;
	unwrap(await commands.firebaseSignOut());
}

export async function firebaseGetUser(): Promise<FirebaseUser | null> {
	if (!isTauri()) return null;
	return unwrap(await commands.firebaseGetUser());
}

export async function cloudGetCredits(): Promise<CreditBalance> {
	if (!isTauri())
		return {
			balance: 0,
			used_this_month: 0,
			plan: "disabled",
			bonus_pool: 0,
			resets_at: "",
		};
	return unwrap(await commands.cloudGetCredits());
}

// --- Agent Plans ---

export async function getAgentPlan(input: string): Promise<AgentPlan | null> {
	if (!isTauri()) return null;
	return unwrap(await commands.getAgentPlan(input));
}

export async function storeAgentPlan(plan: AgentPlan): Promise<void> {
	if (!isTauri()) return;
	unwrap(await commands.storeAgentPlan(plan));
}

export async function executeAgentPlan(planId: string): Promise<void> {
	if (!isTauri()) return;
	unwrap(await commands.executeAgentPlan(planId));
}

// --- Media (MPRIS) ---

export async function mediaGetStatus(): Promise<TrackInfo[]> {
	if (!isTauri()) return [];
	return unwrap(await commands.mediaGetStatus());
}

export async function mediaControl(busName: string, action: string): Promise<void> {
	if (!isTauri()) return;
	unwrap(await commands.mediaControl(busName, action));
}

export async function mediaControlAll(action: string): Promise<number> {
	if (!isTauri()) return 0;
	return unwrap(await commands.mediaControlAll(action));
}

export async function mediaSeek(
	busName: string,
	trackId: string,
	positionUs: number,
): Promise<void> {
	if (!isTauri()) return;
	unwrap(await commands.mediaSeek(busName, trackId, positionUs));
}

export async function mediaRefresh(): Promise<void> {
	if (!isTauri()) return;
	unwrap(await commands.mediaRefresh());
}

// --- Notes & Todos ---

export async function getNotes(): Promise<NoteItem[]> {
	if (!isTauri()) return [];
	return unwrap(await commands.getNotes());
}

export async function addNote(text: string): Promise<NoteItem> {
	if (!isTauri()) return { id: "", text, created_at: 0, updated_at: 0 };
	return unwrap(await commands.addNote(text));
}

export async function updateNote(id: string, text: string): Promise<void> {
	if (!isTauri()) return;
	unwrap(await commands.updateNote(id, text));
}

export async function deleteNote(id: string): Promise<void> {
	if (!isTauri()) return;
	unwrap(await commands.deleteNote(id));
}

export async function getTodos(): Promise<TodoItem[]> {
	if (!isTauri()) return [];
	return unwrap(await commands.getTodos());
}

export async function addTodo(text: string): Promise<TodoItem> {
	if (!isTauri()) return { id: "", text, done: false };
	return unwrap(await commands.addTodo(text));
}

export async function toggleTodo(id: string): Promise<void> {
	if (!isTauri()) return;
	unwrap(await commands.toggleTodo(id));
}

export async function deleteTodo(id: string): Promise<void> {
	if (!isTauri()) return;
	unwrap(await commands.deleteTodo(id));
}

// --- Timer ---

export async function getTimers(): Promise<TimerStatus[]> {
	if (!isTauri()) return [];
	return unwrap(await commands.getTimers());
}

// --- Reminders ---

export async function getReminders(): Promise<ReminderItem[]> {
	if (!isTauri()) return [];
	return unwrap(await commands.getReminders());
}

export async function addReminder(text: string, dueAt: number): Promise<ReminderItem> {
	if (!isTauri()) return { id: "", text, due_at: dueAt, fired: false, created_at: 0 };
	return unwrap(await commands.addReminder(text, dueAt));
}

export async function deleteReminder(id: string): Promise<void> {
	if (!isTauri()) return;
	unwrap(await commands.deleteReminder(id));
}

// --- Snippets ---

export async function getSnippets(): Promise<SnippetItem[]> {
	if (!isTauri()) return [];
	return unwrap(await commands.getSnippets());
}

export async function addSnippet(name: string, body: string): Promise<SnippetItem> {
	if (!isTauri()) return { id: "", name, body, created_at: 0, updated_at: 0 };
	return unwrap(await commands.addSnippet(name, body));
}

export async function updateSnippet(id: string, name: string, body: string): Promise<void> {
	if (!isTauri()) return;
	unwrap(await commands.updateSnippet(id, name, body));
}

export async function deleteSnippet(id: string): Promise<void> {
	if (!isTauri()) return;
	unwrap(await commands.deleteSnippet(id));
}

// --- Context Awareness ---

export async function getContext(): Promise<EnvironmentContext | null> {
	if (!isTauri()) return null;
	return unwrap(await commands.getContext());
}

// --- File Preview ---

export async function getFilePreview(path: string): Promise<FilePreviewData> {
	if (!isTauri())
		return {
			kind: "Unsupported",
			mime: "unknown",
			size_bytes: 0,
			modified_epoch: 0,
			full_path: "",
		};
	return unwrap(await commands.getFilePreview(path));
}

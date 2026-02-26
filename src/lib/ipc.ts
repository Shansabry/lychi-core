import { invoke } from "@tauri-apps/api/core";

export interface CommandResult {
	success: boolean;
	output: string | null;
	error: string | null;
	duration_ms: number;
	routed_by?: string | null;
	/** If set, the frontend should open this URI via GDK for proper Wayland focus. */
	open_url?: string | null;
	/** If set, the action needs user confirmation before executing. */
	needs_confirmation?: string | null;
	/** Risk level of the action (low, medium, high). */
	risk_level?: RiskLevel | null;
	/** How the frontend should render the output. */
	output_type?: OutputType | null;
	/** The actual args executed (set by executor for shell commands). */
	executed_args?: string | null;
}

export interface CompletionItem {
	label: string;
	icon_path: string | null;
	score: number;
	description?: string | null;
	/** Provenance — why this was suggested. Set by context suggestions only. */
	reason?: string | null;
}

function isTauri(): boolean {
	return "__TAURI_INTERNALS__" in window;
}

export async function executeCommand(input: string, confirmed?: boolean): Promise<CommandResult> {
	if (!isTauri()) {
		return { success: false, output: null, error: "Not running in Tauri", duration_ms: 0 };
	}
	return invoke<CommandResult>("execute_command", { input, confirmed: confirmed ?? null });
}

export async function getHistory(): Promise<string[]> {
	if (!isTauri()) return [];
	return invoke<string[]>("get_history");
}

export async function clearHistory(): Promise<void> {
	if (!isTauri()) return;
	return invoke("clear_history");
}

export async function hideWindow(): Promise<void> {
	if (!isTauri()) return;
	const main = document.querySelector("main");
	if (main) {
		main.classList.add("lychi-closing");
		await new Promise((r) => setTimeout(r, 100));
	}
	await invoke("hide_launcher");
	main?.classList.remove("lychi-closing");
}

export async function getHideOnBlur(): Promise<boolean> {
	if (!isTauri()) return true;
	return invoke<boolean>("get_hide_on_blur");
}

export async function getCompletions(input: string): Promise<CompletionItem[]> {
	if (!isTauri()) return [];
	return invoke<CompletionItem[]>("get_completions", { input });
}

export async function listPathCompletions(partial: string): Promise<CompletionItem[]> {
	if (!isTauri()) return [];
	return invoke<CompletionItem[]>("list_path_completions", { partial });
}

export interface DirEntry {
	name: string;
	path: string;
}

export async function listDirectories(path: string): Promise<DirEntry[]> {
	if (!isTauri()) return [];
	return invoke<DirEntry[]>("list_directories", { path });
}

// --- Recursive file search ---

export interface MountPoint {
	path: string;
	label: string;
}

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

export async function getMountPoints(): Promise<MountPoint[]> {
	if (!isTauri()) return [];
	return invoke<MountPoint[]>("get_mount_points");
}

export async function startFileSearch(
	query: string,
	scope: string,
	searchId: number,
): Promise<void> {
	if (!isTauri()) return;
	return invoke("start_file_search", { query, scope, searchId });
}

export async function cancelFileSearch(): Promise<void> {
	if (!isTauri()) return;
	return invoke("cancel_file_search");
}

export async function saveWindowPosition(x: number, y: number): Promise<void> {
	if (!isTauri()) return;
	return invoke("save_window_position", { x, y });
}

export async function openUri(uri: string): Promise<void> {
	if (!isTauri()) return;
	return invoke("open_uri", { uri });
}

// --- Batch settings (single IPC call) ---

export interface AllSettings {
	ai: AiConfig;
	general: GeneralConfig;
	commands: CommandsConfig;
	projects: ProjectsConfig;
	privacy: PrivacyConfig;
	keybindings: KeybindingsConfig;
	app_version: string;
	layer_shell_supported: boolean;
	active_window_strategy: string;
}

export async function getAllSettings(): Promise<AllSettings> {
	if (!isTauri())
		return {
			ai: { mode: "disabled", provider: "anthropic", model: "", ollama_url: "" },
			general: {
				hide_on_blur: true,
				show_duration_ms: true,
				theme: "dark",
				hotkey: "Ctrl+Space",
				window_x: null,
				window_y: null,
				monitor_mode: "cursor",
				window_strategy: "auto",
			},
			commands: {
				default_search_engine: "https://www.google.com/search?q=",
				youtube_url: "https://www.youtube.com/results?search_query=",
				shell: "/bin/bash",
				terminal: "",
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
			},
			app_version: "0.0.0",
			layer_shell_supported: false,
			active_window_strategy: "x11",
		};
	return invoke<AllSettings>("get_all_settings");
}

export interface AllNotes {
	notes: NoteItem[];
	todos: TodoItem[];
}

export async function getAllNotes(): Promise<AllNotes> {
	if (!isTauri()) return { notes: [], todos: [] };
	return invoke<AllNotes>("get_all_notes");
}

// --- General Config ---

export interface GeneralConfig {
	hide_on_blur: boolean;
	show_duration_ms: boolean;
	theme: string;
	hotkey: string;
	window_x: number | null;
	window_y: number | null;
	monitor_mode: string;
	window_strategy: string;
}

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
		};
	return invoke<GeneralConfig>("get_general_config");
}

export async function saveGeneralConfig(general: GeneralConfig): Promise<void> {
	if (!isTauri()) return;
	return invoke("save_general_config", { general });
}

export async function getLayerShellSupported(): Promise<boolean> {
	if (!isTauri()) return false;
	return invoke<boolean>("get_layer_shell_supported");
}

export async function getActiveWindowStrategy(): Promise<string> {
	if (!isTauri()) return "x11";
	return invoke<string>("get_active_window_strategy");
}

export interface CommandsConfig {
	default_search_engine: string;
	youtube_url: string;
	shell: string;
	terminal: string;
}

export async function getCommandsConfig(): Promise<CommandsConfig> {
	if (!isTauri())
		return {
			default_search_engine: "https://www.google.com/search?q=",
			youtube_url: "https://www.youtube.com/results?search_query=",
			shell: "/bin/bash",
			terminal: "",
		};
	return invoke<CommandsConfig>("get_commands_config");
}

export async function saveCommandsConfig(commands: CommandsConfig): Promise<void> {
	if (!isTauri()) return;
	return invoke("save_commands_config", { commands });
}

export interface ProjectsConfig {
	directories: string[];
}

export async function getProjectsConfig(): Promise<ProjectsConfig> {
	if (!isTauri())
		return {
			directories: ["~/Projects", "~/Dev", "~/Code", "~/repos"],
		};
	return invoke<ProjectsConfig>("get_projects_config");
}

export async function saveProjectsConfig(projects: ProjectsConfig): Promise<void> {
	if (!isTauri()) return;
	return invoke("save_projects_config", { projects });
}

// --- Privacy ---

export interface PrivacyConfig {
	allow_ip_geolocation: boolean;
	allow_public_ip: boolean;
}

export async function getPrivacyConfig(): Promise<PrivacyConfig> {
	if (!isTauri()) return { allow_ip_geolocation: false, allow_public_ip: false };
	return invoke<PrivacyConfig>("get_privacy_config");
}

export async function savePrivacyConfig(privacy: PrivacyConfig): Promise<void> {
	if (!isTauri()) return;
	return invoke("save_privacy_config", { privacy });
}

/** C6: Grant a specific privacy consent and persist to config. */
export async function grantPrivacyConsent(feature: string): Promise<void> {
	if (!isTauri()) return;
	return invoke("grant_privacy_consent", { feature });
}

export async function restartApp(): Promise<void> {
	if (!isTauri()) return;
	return invoke("restart_app");
}

export async function setHotkey(hotkey: string): Promise<void> {
	if (!isTauri()) return;
	return invoke("set_hotkey", { hotkey });
}

export async function recordHotkey(): Promise<string> {
	if (!isTauri()) return "";
	return invoke("record_hotkey");
}

// --- Keybindings ---

export interface KeybindingsConfig {
	toggle_history: string;
	toggle_notes: string;
	toggle_media: string;
	toggle_settings: string;
	open_inline_url: string;
	submit: string;
	dismiss: string;
	tab_complete: string;
	tab_back: string;
	switch_scope: string;
}

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
};

export async function getKeybindingsConfig(): Promise<KeybindingsConfig> {
	if (!isTauri()) return { ...KEYBINDINGS_DEFAULTS };
	return invoke<KeybindingsConfig>("get_keybindings_config");
}

export async function saveKeybindingsConfig(keybindings: KeybindingsConfig): Promise<void> {
	if (!isTauri()) return;
	return invoke("save_keybindings_config", { keybindings });
}

// --- AI ---

export interface AiConfig {
	mode: string;
	provider: string;
	model: string;
	ollama_url: string;
}

export interface AiStatus {
	mode: string;
	provider: string;
	model: string;
	has_ai_router: boolean;
}

export async function getAiConfig(): Promise<AiConfig> {
	if (!isTauri()) return { mode: "disabled", provider: "anthropic", model: "", ollama_url: "" };
	return invoke<AiConfig>("get_ai_config");
}

export async function saveAiConfig(aiConfig: AiConfig): Promise<void> {
	if (!isTauri()) return;
	return invoke("save_ai_config", { aiConfig });
}

export async function setApiKey(provider: string, key: string): Promise<void> {
	if (!isTauri()) return;
	return invoke("set_api_key", { provider, key });
}

export async function getMaskedApiKey(provider: string): Promise<string | null> {
	if (!isTauri()) return null;
	return invoke<string | null>("get_masked_api_key", { provider });
}

export async function getAiStatus(): Promise<AiStatus> {
	if (!isTauri()) return { mode: "disabled", provider: "", model: "", has_ai_router: false };
	return invoke<AiStatus>("get_ai_status");
}

export async function checkAiHealth(): Promise<boolean> {
	if (!isTauri()) return false;
	return invoke<boolean>("check_ai_health");
}

// --- Agent Plans ---

export type RiskLevel = "low" | "medium" | "high";
export type OutputType = "terminal" | "text" | "status" | "weather";

export interface AgentStep {
	action_id: string;
	args: string;
	label: string;
	risk: RiskLevel;
}

export interface AgentPlan {
	id: string;
	input: string;
	steps: AgentStep[];
}

export interface StepEvent {
	plan_id: string;
	step_index: number;
	status: "running" | "done" | "failed";
	result?: CommandResult | null;
}

export async function getAgentPlan(input: string): Promise<AgentPlan | null> {
	if (!isTauri()) return null;
	return invoke<AgentPlan | null>("get_agent_plan", { input });
}

export async function storeAgentPlan(plan: AgentPlan): Promise<void> {
	if (!isTauri()) return;
	return invoke("store_agent_plan", { plan });
}

export async function executeAgentPlan(planId: string): Promise<void> {
	if (!isTauri()) return;
	return invoke("execute_agent_plan", { planId });
}

// --- Media (MPRIS) ---

export interface TrackInfo {
	title: string;
	artist: string;
	album: string;
	art_url: string | null;
	/** MPRIS track object path (needed for seek) */
	track_id: string;
	/** Track length in microseconds */
	length_us: number;
	/** Current playback position in microseconds */
	position_us: number;
	status: "playing" | "paused" | "stopped";
	/** D-Bus bus name (e.g. "org.mpris.MediaPlayer2.spotify") */
	bus_name: string;
	/** Friendly player name (e.g. "Spotify", "Firefox") */
	player_name: string;
}

export async function mediaGetStatus(): Promise<TrackInfo[]> {
	if (!isTauri()) return [];
	return invoke<TrackInfo[]>("media_get_status");
}

export async function mediaControl(busName: string, action: string): Promise<void> {
	if (!isTauri()) return;
	return invoke("media_control", { busName, action });
}

export async function mediaControlAll(action: string): Promise<number> {
	if (!isTauri()) return 0;
	return invoke("media_control_all", { action });
}

export async function mediaSeek(
	busName: string,
	trackId: string,
	positionUs: number,
): Promise<void> {
	if (!isTauri()) return;
	return invoke("media_seek", { busName, trackId, positionUs });
}

export async function mediaRefresh(): Promise<void> {
	if (!isTauri()) return;
	return invoke("media_refresh");
}

// --- Notes & Todos ---

export interface NoteItem {
	id: string;
	text: string;
	created_at: number;
	updated_at: number;
}

export interface TodoItem {
	id: string;
	text: string;
	done: boolean;
}

export async function getNotes(): Promise<NoteItem[]> {
	if (!isTauri()) return [];
	return invoke<NoteItem[]>("get_notes");
}

export async function addNote(text: string): Promise<NoteItem> {
	if (!isTauri()) return { id: "", text, created_at: 0, updated_at: 0 };
	return invoke<NoteItem>("add_note", { text });
}

export async function updateNote(id: string, text: string): Promise<void> {
	if (!isTauri()) return;
	return invoke("update_note", { id, text });
}

export async function deleteNote(id: string): Promise<void> {
	if (!isTauri()) return;
	return invoke("delete_note", { id });
}

export async function getTodos(): Promise<TodoItem[]> {
	if (!isTauri()) return [];
	return invoke<TodoItem[]>("get_todos");
}

export async function addTodo(text: string): Promise<TodoItem> {
	if (!isTauri()) return { id: "", text, done: false };
	return invoke<TodoItem>("add_todo", { text });
}

export async function toggleTodo(id: string): Promise<void> {
	if (!isTauri()) return;
	return invoke("toggle_todo", { id });
}

export async function deleteTodo(id: string): Promise<void> {
	if (!isTauri()) return;
	return invoke("delete_todo", { id });
}

// --- Timer ---

export interface TimerStatus {
	id: string;
	name: string;
	duration_secs: number;
	remaining_secs: number;
	elapsed_secs: number;
	paused: boolean;
	done: boolean;
	stopwatch: boolean;
}

export async function getTimers(): Promise<TimerStatus[]> {
	if (!isTauri()) return [];
	return invoke<TimerStatus[]>("get_timers");
}

// --- Reminders ---

export interface ReminderItem {
	id: string;
	text: string;
	due_at: number;
	fired: boolean;
	created_at: number;
}

export async function getReminders(): Promise<ReminderItem[]> {
	if (!isTauri()) return [];
	return invoke<ReminderItem[]>("get_reminders");
}

export async function addReminder(text: string, dueAt: number): Promise<ReminderItem> {
	if (!isTauri()) return { id: "", text, due_at: dueAt, fired: false, created_at: 0 };
	return invoke<ReminderItem>("add_reminder", { text, dueAt });
}

export async function deleteReminder(id: string): Promise<void> {
	if (!isTauri()) return;
	return invoke<void>("delete_reminder", { id });
}

// --- Snippets ---

export interface SnippetItem {
	id: string;
	name: string;
	body: string;
	created_at: number;
	updated_at: number;
}

export async function getSnippets(): Promise<SnippetItem[]> {
	if (!isTauri()) return [];
	return invoke<SnippetItem[]>("get_snippets");
}

export async function addSnippet(name: string, body: string): Promise<SnippetItem> {
	if (!isTauri()) return { id: "", name, body, created_at: 0, updated_at: 0 };
	return invoke<SnippetItem>("add_snippet", { name, body });
}

export async function updateSnippet(id: string, name: string, body: string): Promise<void> {
	if (!isTauri()) return;
	return invoke("update_snippet", { id, name, body });
}

export async function deleteSnippet(id: string): Promise<void> {
	if (!isTauri()) return;
	return invoke("delete_snippet", { id });
}

// --- Context Awareness ---

export interface WindowContext {
	title: string;
	wm_class: string;
	pid: number;
	is_terminal: boolean;
	is_ide: boolean;
}

export interface GitContext {
	repo_root: string;
	branch: string;
	dirty: boolean;
	remote: string | null;
}

export interface ProjectScript {
	runner: string;
	name: string;
}

export interface ProjectContext {
	root: string;
	kind: string;
	has_compose: boolean;
	scripts: ProjectScript[];
}

export interface ContainerInfo {
	id: string;
	name: string;
	image: string;
	status: string;
}

export interface DockerContext {
	containers: ContainerInfo[];
}

export interface EnvironmentContext {
	active_window: WindowContext | null;
	cwd: string | null;
	terminal_cwd: string | null;
	git: GitContext | null;
	project: ProjectContext | null;
	docker: DockerContext | null;
	hour: number;
	gather_ms: number;
}

export async function getContext(): Promise<EnvironmentContext | null> {
	if (!isTauri()) return null;
	return invoke<EnvironmentContext | null>("get_context");
}

// --- File Preview ---

export type FilePreviewData = {
	size_bytes: number;
	modified_epoch: number;
	full_path: string;
} & (
	| { kind: "Text"; content: string; language: string; truncated: boolean }
	| { kind: "Image"; base64: string; mime: string }
	| { kind: "Unsupported"; mime: string }
	| { kind: "Directory"; item_count: number; children: { name: string; is_dir: boolean }[] }
);

export async function getFilePreview(path: string): Promise<FilePreviewData> {
	if (!isTauri())
		return {
			kind: "Unsupported",
			mime: "unknown",
			size_bytes: 0,
			modified_epoch: 0,
			full_path: "",
		};
	return invoke<FilePreviewData>("get_file_preview", { path });
}

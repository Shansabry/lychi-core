import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";

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
}

export interface CompletionItem {
	label: string;
	icon_path: string | null;
	score: number;
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
	await getCurrentWindow().hide();
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

export async function saveWindowPosition(x: number, y: number): Promise<void> {
	if (!isTauri()) return;
	return invoke("save_window_position", { x, y });
}

export async function openUri(uri: string): Promise<void> {
	if (!isTauri()) return;
	return invoke("open_uri", { uri });
}

// --- General Config ---

export interface GeneralConfig {
	hide_on_blur: boolean;
	show_duration_ms: boolean;
	theme: string;
	hotkey: string;
	window_x: number | null;
	window_y: number | null;
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
		};
	return invoke<GeneralConfig>("get_general_config");
}

export async function saveGeneralConfig(general: GeneralConfig): Promise<void> {
	if (!isTauri()) return;
	return invoke("save_general_config", { general });
}

export interface CommandsConfig {
	default_search_engine: string;
	youtube_url: string;
	shell: string;
}

export async function getCommandsConfig(): Promise<CommandsConfig> {
	if (!isTauri())
		return {
			default_search_engine: "https://www.google.com/search?q=",
			youtube_url: "https://www.youtube.com/results?search_query=",
			shell: "/bin/bash",
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

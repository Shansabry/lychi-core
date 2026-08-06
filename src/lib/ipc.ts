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
	AiPresetItem,
	AiStatus,
	AiTestResult,
	AliasItem,
	AllNotes,
	AllSettings,
	CommandInfo,
	CommandsConfig,
	CompletionItem,
	ContentPart,
	Conversation,
	ConversationSummary,
	CreditBalance,
	DirEntry,
	EnvironmentContext,
	FileAttachment,
	FilePreviewData,
	FirebaseUser,
	FontFamily,
	GeneralConfig,
	HotkeyStatus,
	KeybindingsConfig,
	LocalModelInfo,
	MessageDisplay,
	MountPoint,
	NoteItem,
	OllamaModelInfo,
	PrivacyConfig,
	ProjectsConfig,
	ReminderItem,
	RouteDecision,
	ScratchItem,
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
	AiPresetItem,
	AiStatus,
	AiTestResult,
	AliasItem,
	AllNotes,
	AllSettings,
	AttachmentRoute,
	ChatMessage,
	ClipboardContentType,
	CommandInfo,
	CommandsConfig,
	CompletionItem,
	ContainerInfo,
	ContentPart,
	Conversation,
	ConversationSummary,
	CreditBalance,
	DirChild,
	DirEntry,
	DockerContext,
	EnvironmentContext,
	FileAttachment,
	FileKind,
	FilePreviewData,
	FirebaseUser,
	FontFamily,
	GeneralConfig,
	GitContext,
	HotkeyStatus,
	KeybindingsConfig,
	LocalModelInfo,
	MessageDisplay,
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
	Quicklink,
	QuicklinkKind,
	ReminderItem,
	RiskLevel,
	RouteDecision,
	ScratchItem,
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

/**
 * A `lychi://agent-event` payload — the tool-calling agent's unified stream. Kind
 * discriminates the shape (mirrors the Rust `AgentEventDto`; hand-written here
 * because tauri-specta only exports types reachable from command signatures, and
 * this rides an event). Kinds: turn_started | text | reasoning | tool_started |
 * tool_completed | tool_failed | awaiting_approval | final | stopped | error.
 */
export interface AgentEventDto {
	gen: number;
	kind: string;
	text?: string;
	call_id?: string;
	tool_name?: string;
	tool_args?: string;
	reason?: string;
	step: number | null;
	/** final: the answer was cut off at the token cap. */
	truncated?: boolean;
	/** usage: token counts for the turn. */
	input_tokens?: number;
	output_tokens?: number;
	/** tool_completed: a rich artifact to render inline (svg | weather | …). */
	artifact_kind?: string;
	artifact_content?: string;
}

/**
 * A `lychi://ai-on-selection` payload — a global hotkey ran AI on whatever text
 * the user had highlighted. Hand-written here (like `AgentEventDto`) because
 * tauri-specta only exports types reachable from a command signature, and this
 * rides an event.
 */
export interface AiOnSelectionPayload {
	/** The fully rendered prompt for the model. */
	prompt: string;
	/** The instruction shown as the user's message. */
	display: string;
	/** The selected text, folded into a collapsed chip. */
	body: string;
	/** Non-empty when the text came from the clipboard rather than the live
	 *  selection (GNOME Wayland), so the UI can say so. */
	note: string;
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

/**
 * Run a command.
 *
 * `query` is the text the user had TYPED when they picked this command, passed
 * only when a suggestion was SELECTED rather than typed out. It teaches the
 * launcher `query → chosen command`, so correcting a bad ranking once fixes it
 * (see `frecency::record_latch`). Omit it for a directly-typed command: a query
 * that is its own answer teaches nothing.
 */
export async function executeCommand(
	input: string,
	confirmed?: boolean,
	runInline?: boolean,
	query?: string,
): Promise<CommandResult> {
	if (!isTauri()) {
		return {
			success: false,
			output: null,
			error: "Not running in Tauri",
			duration_ms: 0,
		} as CommandResult;
	}
	return unwrap(
		await commands.executeCommand(input, confirmed ?? null, runInline ?? null, query ?? null),
	);
}

export async function confirmExecution(): Promise<CommandResult> {
	if (!isTauri()) {
		return {
			success: false,
			output: null,
			error: "Not running in Tauri",
			duration_ms: 0,
		} as CommandResult;
	}
	return unwrap(await commands.confirmExecution());
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

/**
 * Classify a raw input string into a typed routing decision — the SINGLE source
 * of truth for "what does Enter do?". The frontend actuates the result verbatim
 * (run a command, open a panel, go to the agent/fork card, fill a correction),
 * never re-deriving command-vs-AI from its own keyword list.
 */
export async function classifyInput(input: string): Promise<RouteDecision | undefined> {
	if (!isTauri()) return undefined;
	return unwrap(await commands.classifyInput(input));
}

/**
 * A "Did you mean: X?" correction for a near-miss single word (e.g. "spoti" →
 * "open Spotify"), or null. Used on Enter before falling to the AI, so an app
 * typo is corrected instead of sent to the model. Returns the corrected command.
 */
export async function suggestCorrection(input: string): Promise<string | null> {
	if (!isTauri()) return null;
	return unwrap(await commands.suggestCorrection(input));
}

export async function getCommandCatalog(): Promise<CommandInfo[]> {
	if (!isTauri()) return [];
	return unwrap(await commands.getCommandCatalog());
}

export async function getTriggerCatalog(): Promise<CommandInfo[]> {
	if (!isTauri()) return [];
	return unwrap(await commands.getTriggerCatalog());
}

export async function listPathCompletions(partial: string): Promise<CompletionItem[]> {
	if (!isTauri()) return [];
	return unwrap(await commands.listPathCompletions(partial));
}

export async function fuzzyPathCompletions(query: string): Promise<CompletionItem[]> {
	if (!isTauri()) return [];
	return unwrap(await commands.fuzzyPathCompletions(query));
}

export async function listDirectories(path: string): Promise<DirEntry[]> {
	if (!isTauri()) return [];
	return unwrap(await commands.listDirectories(path));
}

/**
 * Flatten a `ChatMessage.content` block list to its prose. Since vision landed,
 * `content` is a `ContentPart[]` (text interleaved with images) rather than a
 * string; anything that renders a stored message needs the text half. Mirrors
 * the Rust `ChatMessage::content_text()` — keep the two in step.
 */
export function contentText(content: ContentPart[]): string {
	return content
		.filter((p) => p.type === "text")
		.map((p) => p.text)
		.join("");
}

/**
 * Classify attached file paths into chip-ready descriptors. The backend decides
 * kind/mime/thumbnail AND which pipe each file takes (`route`) — the frontend
 * only renders the chips and forwards the paths on submit.
 */
export async function classifyFiles(paths: string[]): Promise<FileAttachment[]> {
	if (!isTauri()) return [];
	return unwrap(await commands.classifyFiles(paths));
}

/**
 * Whether the selected model accepts images. `"unknown"` means no evidence —
 * callers must ALLOW the attempt (refusing on absent evidence would block every
 * newly released vision model). Learned from provider metadata or a previously
 * observed rejection; never a hardcoded model list.
 */
export type ModelVision = "supported" | "unsupported" | "unknown";

export async function getModelVision(): Promise<ModelVision> {
	if (!isTauri()) return "unknown";
	return unwrap(await commands.getModelVision()) as ModelVision;
}

/**
 * Stage whatever the clipboard holds as attachments (the paste gesture): copied
 * files, or copied image data spilled to disk. Copied TEXT returns nothing —
 * that belongs in the input box, so the browser's own paste handles it.
 */
export async function attachFromClipboard(): Promise<FileAttachment[]> {
	if (!isTauri()) return [];
	return unwrap(await commands.attachFromClipboard());
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
				base_url: "",
				wire_format: "",
				ollama_url: "",
				ollama_model: "",
				local_model: "",
				timeout_secs: 8,
				max_tokens: 300,
			},
			general: {
				hide_on_blur: true,
				show_duration_ms: true,
				theme: "dark",
				accent: "",
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
				extra_terminals: [],
				extra_ides: [],
				terminal_routing: "manual",
				search_engines: {},
			},
			projects: {
				directories: [],
				extra_strong_markers: [],
				extra_soft_markers: [],
				pinned_workspace: null,
			},
			privacy: structuredClone(PRIVACY_DEFAULTS),
			// One source of truth — this literal used to be a second copy that had
			// to be updated in lockstep whenever a binding was added.
			keybindings: { ...KEYBINDINGS_DEFAULTS },
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
			accent: "",
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
			extra_terminals: [],
			extra_ides: [],
			terminal_routing: "manual",
			search_engines: {},
			quicklinks: [],
		};
	return unwrap(await commands.getCommandsConfig());
}

/// Keywords already taken by built-in commands, from the backend's live action
/// registry. The Settings UI uses this for an instant collision warning instead
/// of keeping its own copy, which would drift as handlers are added.
export async function getReservedKeywords(): Promise<string[]> {
	if (!isTauri()) return [];
	return unwrap(await commands.getReservedKeywords());
}

/// Font families installed on this system, for the Settings font pickers. The
/// WebView can't enumerate these itself, so the list comes from fontconfig.
export async function getInstalledFonts(): Promise<FontFamily[]> {
	if (!isTauri()) return [];
	return unwrap(await commands.getInstalledFonts());
}

// --- Aliases ---

export async function getAliases(): Promise<AliasItem[]> {
	if (!isTauri()) return [];
	return unwrap(await commands.getAliases());
}

export async function addAlias(name: string, command: string): Promise<AliasItem> {
	return unwrap(await commands.addAlias(name, command));
}

export async function updateAlias(name: string, command: string): Promise<void> {
	unwrap(await commands.updateAlias(name, command));
}

export async function deleteAlias(name: string): Promise<void> {
	unwrap(await commands.deleteAlias(name));
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
			extra_strong_markers: [],
			extra_soft_markers: [],
			pinned_workspace: null,
		};
	return unwrap(await commands.getProjectsConfig());
}

export async function saveProjectsConfig(projects: ProjectsConfig): Promise<void> {
	if (!isTauri()) return;
	unwrap(await commands.saveProjectsConfig(projects));
}

// --- Privacy ---

/// Mirrors `PrivacyConfig::default()` in Rust. Network consents default off;
/// clipboard exclusions default *on*, because a secret that reaches the history
/// is already leaked (C6).
export const PRIVACY_DEFAULTS: PrivacyConfig = {
	allow_ip_geolocation: false,
	allow_public_ip: false,
	clipboard: { respect_sensitive_hint: true, excluded_apps: [] },
};

export async function getPrivacyConfig(): Promise<PrivacyConfig> {
	if (!isTauri()) return structuredClone(PRIVACY_DEFAULTS);
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
	action_panel: "Ctrl+K",
	attach_file: "Ctrl+Shift+A",
	approve_action: "Ctrl+Enter",
	reject_action: "Escape",
	// Same binding as `web_search` on purpose: same intent, different surface.
	fork_web: "Ctrl+Enter",
	fork_chat: "Enter",
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
			base_url: "",
			wire_format: "",
			ollama_url: "",
			ollama_model: "",
			local_model: "",
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

/** Cancel any in-flight AI chat stream. */
export async function cancelAiChat(): Promise<void> {
	if (!isTauri()) return;
	unwrap(await commands.cancelAiChat());
}

/**
 * Start a tool-calling agent chat (Phase 2). Seeds a session from a system +
 * user prompt and drives the coordinator loop; progress arrives via the
 * `lychi://agent-event` event. May pause on a destructive tool (an
 * `awaiting_approval` event) — resolve with `agentApprove`.
 */
export async function agentChatStart(
	system: string,
	user: string,
	fresh: boolean,
	withTools: boolean,
	generation: number,
	images: string[] = [],
	display: MessageDisplay | null = null,
): Promise<void> {
	if (!isTauri()) return;
	// The command now takes a named object (see AgentChatStart in the backend):
	// four of these are bool/number/string, so a transposed pair used to compile
	// and fail at runtime. This wrapper keeps its positional signature because
	// every caller already uses it and the names are bound right here.
	unwrap(
		await commands.agentChatStart({
			system,
			user,
			fresh,
			withTools,
			generation,
			images,
			display,
		}),
	);
}

/** Approve (or reject) a pending destructive tool call, resuming the agent. */
export async function agentApprove(approve: boolean, generation: number): Promise<void> {
	if (!isTauri()) return;
	unwrap(await commands.agentApprove(approve, generation));
}

export async function testAiConnection(): Promise<AiTestResult> {
	if (!isTauri()) return { ok: false, error: "Not running in Tauri" };
	return unwrap(await commands.testAiConnection());
}

export async function listOllamaModels(): Promise<OllamaModelInfo[]> {
	if (!isTauri()) return [];
	return unwrap(await commands.listOllamaModels());
}

// --- Local AI (bundled CPU inference) ---

export async function getLocalModels(): Promise<LocalModelInfo[]> {
	if (!isTauri()) return [];
	return unwrap(await commands.getLocalModels());
}

/** Starts a background download; progress arrives via onModelDownloadProgress. */
export async function downloadLocalModel(modelId: string): Promise<void> {
	if (!isTauri()) return;
	unwrap(await commands.downloadLocalModel(modelId));
}

export async function deleteLocalModel(modelId: string): Promise<void> {
	if (!isTauri()) return;
	unwrap(await commands.deleteLocalModel(modelId));
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

// --- Unified scratch surface (notes + todos merged) ---

export async function getAllItems(): Promise<ScratchItem[]> {
	if (!isTauri()) return [];
	return unwrap(await commands.getAllItems());
}

export async function toggleItem(id: string): Promise<void> {
	if (!isTauri()) return;
	unwrap(await commands.toggleItem(id));
}

export async function deleteItem(id: string): Promise<void> {
	if (!isTauri()) return;
	unwrap(await commands.deleteItem(id));
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

// --- AI Presets (AI Commands) ---

export async function getAiPresets(): Promise<AiPresetItem[]> {
	if (!isTauri()) return [];
	return unwrap(await commands.getAiPresets());
}

export async function addAiPreset(
	keyword: string,
	name: string,
	template: string,
): Promise<AiPresetItem> {
	if (!isTauri()) return { id: "", keyword, name, template, created_at: 0, updated_at: 0 };
	return unwrap(await commands.addAiPreset(keyword, name, template));
}

export async function updateAiPreset(
	id: string,
	keyword: string,
	name: string,
	template: string,
): Promise<void> {
	if (!isTauri()) return;
	unwrap(await commands.updateAiPreset(id, keyword, name, template));
}

export async function deleteAiPreset(id: string): Promise<void> {
	if (!isTauri()) return;
	unwrap(await commands.deleteAiPreset(id));
}

// --- AI Conversation History (recall) ---

export async function getConversations(): Promise<ConversationSummary[]> {
	if (!isTauri()) return [];
	return unwrap(await commands.getConversations());
}

export async function getConversation(id: string): Promise<Conversation | null> {
	if (!isTauri()) return null;
	return unwrap(await commands.getConversation(id));
}

export async function deleteConversation(id: string): Promise<void> {
	if (!isTauri()) return;
	unwrap(await commands.deleteConversation(id));
}

export async function clearConversations(): Promise<void> {
	if (!isTauri()) return;
	unwrap(await commands.clearConversations());
}

/** Prime the agent session with a stored conversation so it can be continued. */
export async function loadConversation(id: string): Promise<Conversation | null> {
	if (!isTauri()) return null;
	return unwrap(await commands.loadConversation(id));
}

// --- Context Awareness ---

export async function getContext(): Promise<EnvironmentContext | null> {
	if (!isTauri()) return null;
	return unwrap(await commands.getContext());
}

/**
 * Read the PRIMARY selection — text the user has highlighted (not copied) in the
 * focused window. Returns null if nothing is selected. Used to auto-fill AI
 * commands: `summarize` with no typed text acts on the selection.
 */
export async function readSelection(): Promise<string | null> {
	if (!isTauri()) return null;
	return unwrap(await commands.readSelection());
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

/**
 * Run an action declared on a result row.
 *
 * The row carries `{id, target}` and the id of the handler that produced it —
 * never a command string. The backend resolves that triple to a command,
 * validating both the verb and the target against what the handler actually
 * emitted, and then runs it through the same executor path as typed input. So a
 * row action is a proposal, not a bypass: risk assessment, confirmation and the
 * denylist all still apply.
 */
export async function runRowAction(
	handler: string,
	id: string,
	target: string,
): Promise<CommandResult> {
	if (!isTauri()) {
		return { success: false, error: "Not running in Tauri", duration_ms: 0, auto_open: false };
	}
	return unwrap(await commands.runRowAction(handler, id, target));
}

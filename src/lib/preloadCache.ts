/**
 * Preload cache — eagerly fetches settings and notes data at app startup
 * so panels open instantly without waiting for IPC calls.
 *
 * Uses batch IPC commands (get_all_settings, get_all_notes) to fetch
 * everything in a single round-trip instead of 10+ separate calls.
 */
import type {
	AiConfig,
	AllNotes,
	AllSettings,
	CommandsConfig,
	GeneralConfig,
	KeybindingsConfig,
	PrivacyConfig,
	ProjectsConfig,
} from "$lib/ipc";
import { getAllNotes, getAllSettings } from "$lib/ipc";

// --- Settings cache ---

export interface SettingsCache {
	aiConfig: AiConfig;
	generalConfig: GeneralConfig;
	commandsConfig: CommandsConfig;
	projectsConfig: ProjectsConfig;
	privacyConfig: PrivacyConfig;
	keybindingsConfig: KeybindingsConfig;
	appVersion: string;
	layerShellSupported: boolean;
	activeWindowStrategy: string;
}

let settingsPromise: Promise<SettingsCache> | null = null;

export function preloadSettings(): Promise<SettingsCache> {
	if (!settingsPromise) {
		settingsPromise = getAllSettings().then((s: AllSettings) => ({
			aiConfig: s.ai,
			generalConfig: s.general,
			commandsConfig: s.commands,
			projectsConfig: s.projects,
			privacyConfig: s.privacy,
			keybindingsConfig: s.keybindings,
			appVersion: s.app_version,
			layerShellSupported: s.layer_shell_supported,
			activeWindowStrategy: s.active_window_strategy,
		}));
	}
	return settingsPromise;
}

/** Invalidate settings cache (call after saving settings). */
export function invalidateSettings() {
	settingsPromise = null;
}

// --- Notes cache ---

export interface NotesCache {
	notes: AllNotes["notes"];
	todos: AllNotes["todos"];
}

let notesPromise: Promise<NotesCache> | null = null;

export function preloadNotes(): Promise<NotesCache> {
	if (!notesPromise) {
		notesPromise = getAllNotes();
	}
	return notesPromise;
}

/** Invalidate notes cache (call after mutation or external change). */
export function invalidateNotes() {
	notesPromise = null;
}

// --- Preload all ---

export function preloadAll(): Promise<[SettingsCache, NotesCache]> {
	return Promise.all([preloadSettings(), preloadNotes()]);
}

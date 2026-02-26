<script lang="ts">
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { onMount } from "svelte";
import AgentPlanPanel from "$lib/components/AgentPlanPanel.svelte";
import CommandInput from "$lib/components/CommandInput.svelte";
import CompletionsList from "$lib/components/CompletionsList.svelte";
import FilePreview from "$lib/components/FilePreview.svelte";
import HistoryPanel from "$lib/components/HistoryPanel.svelte";
import MediaPanel from "$lib/components/MediaPanel.svelte";
import NotesPanel from "$lib/components/NotesPanel.svelte";
import ResultPanel from "$lib/components/ResultPanel.svelte";
import SettingsPanel from "$lib/components/SettingsPanel.svelte";
import StatusBar from "$lib/components/StatusBar.svelte";
import type {
	AgentPlan,
	CommandResult,
	CompletionItem,
	EnvironmentContext,
	FileSearchBatch,
	MountPoint,
	StepEvent,
	TrackInfo,
} from "$lib/ipc";
import {
	cancelFileSearch,
	executeCommand,
	getActiveWindowStrategy,
	getAgentPlan,
	getCompletions,
	getContext,
	getHideOnBlur,
	getHistory,
	getMountPoints,
	grantPrivacyConsent,
	hideWindow,
	listPathCompletions,
	mediaGetStatus,
	openUri,
	saveWindowPosition,
	startFileSearch,
} from "$lib/ipc";
import { loadKeybindings, matchesAction } from "$lib/keybindings";
import { preloadAll } from "$lib/preloadCache";

let inputValue = $state("");
let isExecuting = $state(false);
let isRouting = $state(false);
let backendReady = $state(false);
let lastResult: CommandResult | null = $state(null);
let lastCommand = $state("");

let historyEntries: string[] = $state([]);
let historyOpen = $state(false);

let completions: CompletionItem[] = $state([]);
let completionIndex = $state(-1);
let debounceTimer: ReturnType<typeof setTimeout> | undefined;
let completionGen = 0;

// @ file reference mode (browse — point to a file in a command)
let atMode = $state(false);
let atStart = $state(-1);
let atNoResults = $state(false);

// / file search mode (find and open a file, like Spotlight)
let searchMode = $state(false);
let fileSearchId = $state(0);
let searchDone = $state(false);
let mountPoints: MountPoint[] = $state([]);
let scopeIndex = $state(0);
let activeScope = $derived(mountPoints[scopeIndex]?.path ?? "");
let searchScopePath = $state(""); // absolute path when drilled into a folder
let filePathMap: Map<string, string> = $state(new Map()); // label → full_path for preview
let fileMetaMap: Map<string, { size_bytes?: number | null; modified_secs?: number | null }> =
	$state(new Map());
let ignoreActive = $state(false);

// Derive breadcrumb path context for / search mode (when drilled into a folder)
let searchPathContext = $derived.by(() => {
	if (!searchMode || !searchScopePath) return "";
	const home = mountPoints[0]?.path ?? "";
	if (home && searchScopePath.startsWith(home)) {
		return `~${searchScopePath.slice(home.length)}/`;
	}
	return `${searchScopePath}/`;
});

// Derive breadcrumb path context for @ mode
let atPathContext = $derived.by(() => {
	if (!atMode || atStart < 0) return "";
	const partial = inputValue.slice(atStart + 1);
	const lastSlash = partial.lastIndexOf("/");
	if (lastSlash === -1) return "~/";
	const raw = partial.slice(0, lastSlash + 1);
	return raw.startsWith("/") ? raw : `~/${raw}`;
});

let settingsOpen = $state(false);
let hideOnBlur = $state(true);
let blurEnabled = $state(false);
let windowStrategy = $state("x11"); // "layer-shell" or "x11" — drives layout mode

let pendingPlan: AgentPlan | null = $state(null);
let planPanelRef: AgentPlanPanel | undefined = $state(undefined);
// C1/C15: generation counter for AI routing — ESC increments to cancel stale responses
let routingGeneration = 0;
let mediaOpen = $state(false);
let notesOpen = $state(false);
let pendingNoteText: string | null = $state(null);
let initialNotesTab: "notes" | "todos" | "reminders" | "timers" | "snippets" | undefined =
	$state(undefined);
let mediaPlayers: TrackInfo[] = $state([]);
let envContext: EnvironmentContext | null = $state(null);
let contextPill = $derived.by(() => {
	// Only show context pill for terminal/IDE — not browsers or random apps
	const w = envContext?.active_window;
	if (!w?.is_terminal && !w?.is_ide) return "";
	const ideIsFocused = w?.is_ide ?? false;
	const cwd = ideIsFocused
		? (envContext?.cwd ?? envContext?.terminal_cwd)
		: (envContext?.terminal_cwd ?? envContext?.cwd);
	const folder = cwd?.split("/").pop();
	if (!folder) return "";
	const branch = envContext?.git?.branch;
	if (branch) return `${folder} · ${branch}`;
	return folder;
});
let mediaPollTimer: ReturnType<typeof setTimeout> | undefined;

// Derive the "now playing" track — first playing, or first in list
let nowPlaying = $derived.by(() => {
	const playing = mediaPlayers.find((p) => p.status === "playing");
	return playing ?? mediaPlayers[0] ?? null;
});

let multiplePlayers = $derived(mediaPlayers.length > 1);

// File preview — show for files and folders in search or browse mode
let previewPath = $derived.by(() => {
	if (!searchMode && !atMode) return "";
	if (completions.length === 0 || completionIndex < 0) return "";
	const item = completions[completionIndex];
	if (!item) return "";
	return resolveFullPath(item.label);
});
let showPreview = $derived(previewPath.length > 0);

// Poll media player status — 5s when playing/paused, 30s when idle
$effect(() => {
	function scheduleNext() {
		const hasActive = mediaPlayers.some((p) => p.status === "playing" || p.status === "paused");
		mediaPollTimer = setTimeout(poll, hasActive ? 5000 : 30000);
	}

	async function poll() {
		try {
			const players = await mediaGetStatus();
			mediaPlayers = players.filter((p) => p.title || p.status !== "stopped");
		} catch {
			mediaPlayers = [];
		}
		scheduleNext();
	}

	poll();
	return () => clearTimeout(mediaPollTimer);
});

// Immediate @ mode completion fetch (no debounce) — used when drilling into dirs
async function fetchAtCompletions(partial: string) {
	try {
		const results = await listPathCompletions(partial);
		// Defer state update to next frame so it never blocks a keystroke paint
		requestAnimationFrame(() => {
			completions = results;
			completionIndex = results.length > 0 ? 0 : -1;
			atNoResults = results.length === 0 && partial.length > 0;
		});
		// Stay in @ mode even when empty — user can backspace to navigate up
	} catch (err) {
		console.error("[@completions] error:", err);
		// Stay in @ mode on error too — don't kick the user out
		requestAnimationFrame(() => {
			completions = [];
			completionIndex = -1;
			atNoResults = true;
		});
	}
}

// Resolve the absolute path for a completion label (for preview)
function resolveFullPath(label: string): string {
	const mapped = filePathMap.get(label);
	if (mapped) return mapped;
	// Browse mode: labels like "~/foo/bar.txt"
	if (label.startsWith("~/")) {
		const home = mountPoints[0]?.path ?? "";
		return `${home}/${label.slice(2)}`;
	}
	return label.endsWith("/") ? label.slice(0, -1) : label;
}

// Called by CommandInput on every keystroke
function handleInput(val: string) {
	// Skip completions until backend is ready — input still updates visually via bind:value
	if (!backendReady) return;
	// Close any open panel so CompletionsList renders
	if (settingsOpen || notesOpen || mediaOpen) {
		settingsOpen = false;
		notesOpen = false;
		mediaOpen = false;
	}
	historyOpen = false;
	clearTimeout(debounceTimer);
	atNoResults = false;

	// Detect / file search mode — must be at the start of input
	if (val.startsWith("/")) {
		const raw = val.slice(1);

		// Parse path first: /folder/subfolder/query → scope=folder/subfolder, searchTerm=query
		const lastSlash = raw.lastIndexOf("/");
		const searchTermCandidate = lastSlash >= 0 ? raw.slice(lastSlash + 1) : raw;

		// Only reject if the search term (part after last /) has a space — folder paths can have spaces
		if (!searchTermCandidate.includes(" ")) {
			// Exit @ mode if active
			atMode = false;
			atStart = -1;

			// Refresh mount points on entering search mode (detects new USB drives etc.)
			if (!searchMode) {
				getMountPoints().then((mounts) => {
					mountPoints = mounts;
				});
			}

			searchMode = true;
			searchDone = false;
			fileSearchId++;
			const id = fileSearchId;

			let searchScope: string;
			let searchTerm: string;
			if (lastSlash >= 0) {
				const folderPart = raw.slice(0, lastSlash); // e.g. "Documents/Agent agnes"
				searchTerm = searchTermCandidate; // e.g. "q1" or ""
				const baseScope = activeScope || (mountPoints[0]?.path ?? "");
				searchScope = folderPart ? `${baseScope}/${folderPart}` : baseScope;
				searchScopePath = searchScope;
			} else {
				searchTerm = raw;
				searchScope = activeScope || (mountPoints[0]?.path ?? "");
				searchScopePath = "";
			}

			// Don't clear completions — keep showing old results until new ones arrive
			if (raw.length > 0 || lastSlash >= 0) {
				debounceTimer = setTimeout(() => {
					completions = [];
					completionIndex = -1;
					if (searchScope) startFileSearch(searchTerm, searchScope, id);
				}, 150);
			} else {
				// Just "/" typed — list the active scope immediately (home dir by default)
				completions = [];
				completionIndex = -1;
				if (searchScope) startFileSearch("", searchScope, id);
			}
			return;
		}
	}

	// Exiting search mode
	if (searchMode) {
		cancelFileSearch();
		searchScopePath = "";
		searchMode = false;
		filePathMap = new Map();
		fileMetaMap = new Map();
		ignoreActive = false;
	}

	// Detect @ file reference — find the last @ in the input
	const atIdx = val.lastIndexOf("@");
	if (atIdx !== -1) {
		const partial = val.slice(atIdx + 1);
		// Skip if it looks like an email (non-space chars before @)
		const beforeAt = val.slice(0, atIdx);
		const isEmail = beforeAt.length > 0 && !beforeAt.endsWith(" ");
		if (!partial.includes(" ") && !isEmail) {
			atMode = true;
			atStart = atIdx;
			cancelFileSearch();
			const gen = ++completionGen;
			listPathCompletions(partial)
				.then((results) => {
					if (gen !== completionGen) return;
					requestAnimationFrame(() => {
						completions = results;
						completionIndex = results.length > 0 ? 0 : -1;
						atNoResults = results.length === 0 && partial.length > 0;
					});
				})
				.catch((err) => {
					console.error("[@completions] error:", err);
					if (gen !== completionGen) return;
					requestAnimationFrame(() => {
						completions = [];
						completionIndex = -1;
						atNoResults = true;
					});
				});
			return;
		}
	}

	// Not in @ or / mode — normal completions
	atMode = false;
	atStart = -1;

	const trimmed = val.trim();
	if (trimmed.length < 1 && !envContext) {
		completionGen++;
		completions = [];
		completionIndex = -1;
		return;
	}

	const gen = ++completionGen;
	getCompletions(trimmed)
		.then((results) => {
			if (gen !== completionGen) return;
			completions = results;
			completionIndex = results.length > 0 ? 0 : -1;
		})
		.catch((err) => {
			console.error("[completions] error:", err);
		});
}

onMount(() => {
	getActiveWindowStrategy().then((s) => {
		windowStrategy = s;
	});
	getHideOnBlur().then((v) => {
		hideOnBlur = v;
	});
	getMountPoints().then((mounts) => {
		mountPoints = mounts;
	});
	getHistory().then((entries) => {
		historyEntries = entries;
	});
	Promise.all([preloadAll(), getCompletions("__warmup__").catch(() => {})])
		.then(([[settings]]) => {
			loadKeybindings(settings.keybindingsConfig);
		})
		.finally(() => {
			backendReady = true;
			if (inputValue.trim()) handleInput(inputValue);
		});

	// Guard: only attach Tauri listeners if running inside Tauri
	if (!("__TAURI_INTERNALS__" in window)) return;

	// Grace period — ignore blur events during startup (WMs can fire blur during show)
	setTimeout(() => {
		blurEnabled = true;
	}, 500);

	let unlisteners: (() => void)[] = [];

	(async () => {
		const win = getCurrentWindow();

		// Listen for agent step events
		const unlistenStep = await win.listen<StepEvent>("lychi://agent-step", (e) => {
			planPanelRef?.handleStepEvent(e.payload);
			// When the final step completes, expose its result to the StatusBar
			// so the AI sparkle indicator shows for AI-routed plans.
			const { result, status } = e.payload;
			if (result && (status === "done" || status === "failed")) {
				lastResult = { ...result, routed_by: "ai" };
			}
		});
		unlisteners.push(unlistenStep);

		// Listen for summon event from Rust (global shortcut / IPC toggle)
		const unlistenSummon = await win.listen("lychi://summon", () => {
			inputValue = "";
			lastResult = null;
			historyOpen = false;
			settingsOpen = false;
			pendingPlan = null;
			mediaOpen = false;
			notesOpen = false;

			completions = [];
			completionIndex = -1;
			atMode = false;
			atStart = -1;
			searchMode = false;
			// C15: Cancel any in-flight AI routing on summon reset
			routingGeneration++;
			isRouting = false;
			cancelFileSearch();
			// Re-arm blur after grace period on each summon
			blurEnabled = false;
			setTimeout(() => {
				blurEnabled = true;
			}, 300);
			// Force focus the input (layer shell may not auto-focus DOM elements)
			requestAnimationFrame(() => {
				document.querySelector<HTMLInputElement>(".input-container input")?.focus();
			});
		});
		unlisteners.push(unlistenSummon);

		// Listen for context-ready event from async context gathering
		const unlistenContext = await win.listen<EnvironmentContext>("lychi://context-ready", (e) => {
			envContext = e.payload;
			// Fetch context suggestions if input is empty
			const input = document.querySelector<HTMLInputElement>(".input-container input");
			if (input && input.value.trim().length < 1) {
				getCompletions("")
					.then((results) => {
						completions = results;
						completionIndex = results.length > 0 ? 0 : -1;
					})
					.catch(() => {});
			}
		});
		unlisteners.push(unlistenContext);

		// Listen for media track updates from background D-Bus listener
		const unlistenMedia = await win.listen<TrackInfo>("lychi://media-track", (e) => {
			const incoming = e.payload;
			const idx = mediaPlayers.findIndex((p) => p.bus_name === incoming.bus_name);
			if (idx >= 0) {
				mediaPlayers[idx] = incoming;
				mediaPlayers = [...mediaPlayers];
			} else {
				mediaPlayers = [...mediaPlayers, incoming];
			}
		});
		unlisteners.push(unlistenMedia);

		// Listen for streaming file search results
		const unlistenFileSearch = await win.listen<FileSearchBatch>(
			"lychi://file-search-results",
			(e) => {
				const batch = e.payload;
				if (batch.search_id !== fileSearchId) return;

				// Populate path map for preview and metadata map for display
				const pathUpdates = new Map(filePathMap);
				const metaUpdates = new Map(fileMetaMap);
				for (const r of batch.results) {
					pathUpdates.set(r.label, r.full_path);
					metaUpdates.set(r.label, { size_bytes: r.size_bytes, modified_secs: r.modified_secs });
				}

				const newItems: CompletionItem[] = batch.results.map((r) => ({
					label: r.label,
					icon_path: r.is_dir ? "__folder__" : null,
					score: r.score,
					description: r.description ?? null,
				}));

				const isDone = batch.done;

				// Defer state update to next frame so it never blocks a keystroke paint
				requestAnimationFrame(() => {
					filePathMap = pathUpdates;
					fileMetaMap = metaUpdates;
					if (batch.has_ignore_rules) ignoreActive = true;

					completions = [...completions, ...newItems]
						.sort((a, b) => {
							const aDir = a.icon_path === "__folder__" ? 1 : 0;
							const bDir = b.icon_path === "__folder__" ? 1 : 0;
							if (bDir !== aDir) return bDir - aDir; // folders first
							return b.score - a.score; // then by score
						})
						.slice(0, 20);

					if (completions.length > 0 && completionIndex < 0) {
						completionIndex = 0;
					}

					if (isDone) {
						searchDone = true;
						atNoResults = completions.length === 0;
					}
				});
			},
		);
		unlisteners.push(unlistenFileSearch);

		// Ctrl+Shift+I: disable blur so devtools can open
		window.addEventListener("keydown", (e) => {
			if (e.ctrlKey && e.shiftKey && e.key === "I") {
				blurEnabled = false;
			}
		});

		// Prevent WebKitGTK from stealing tab_back for native focus navigation
		window.addEventListener(
			"keydown",
			(e) => {
				if (matchesAction(e, "tab_back")) {
					e.preventDefault();
				}
			},
			true,
		);

		// Hide on blur — dismiss when focus leaves the window (click outside, Alt+Tab, etc.).
		let dismissing = false;
		function handleBlurDismiss() {
			if (dismissing || !hideOnBlur || !blurEnabled) return;
			// If a panel is open, close it first instead of dismissing the whole launcher
			if (settingsOpen || notesOpen) {
				settingsOpen = false;
				notesOpen = false;
				return;
			}
			dismissing = true;
			blurEnabled = false; // Prevent re-trigger during hide animation
			// Hide immediately (no closing animation) — blur dismiss should feel instant.
			// State cleanup happens after hide so the user never sees completions vanish first.
			invoke("hide_launcher")
				.then(() => {
					if (searchMode) {
						cancelFileSearch();
						searchMode = false;
					}
					completions = [];
					completionIndex = -1;
					atMode = false;
					atStart = -1;
				})
				.finally(() => {
					dismissing = false;
				});
		}

		const unlistenFocus = await win.onFocusChanged(({ payload: focused }) => {
			if (!focused) handleBlurDismiss();
		});
		unlisteners.push(unlistenFocus);

		// GTK-level focus-out — catches blur when Tauri's JS bridge misses it
		const unlistenGtkBlur = await win.listen("lychi://gtk-blur", () => {
			handleBlurDismiss();
		});
		unlisteners.push(unlistenGtkBlur);

		// Focus watchdog — catches compositors that don't fire keyboard.leave
		const unlistenWatchdog = await win.listen("lychi://focus-watchdog-dismiss", () => {
			handleBlurDismiss();
		});
		unlisteners.push(unlistenWatchdog);

		// Persist window position on move (debounced).
		// Skip (0,0) — Wayland doesn't report real positions.
		let moveTimer: ReturnType<typeof setTimeout> | undefined;
		const unlistenMoved = await win.onMoved(({ payload: pos }) => {
			if (pos.x === 0 && pos.y === 0) return;
			clearTimeout(moveTimer);
			moveTimer = setTimeout(() => {
				saveWindowPosition(pos.x, pos.y);
			}, 500);
		});
		unlisteners.push(unlistenMoved);
	})();

	return () => {
		for (const fn of unlisteners) fn();
	};
});

async function handleSubmit(opts?: { ctrlKey?: boolean }) {
	const trimmed = inputValue.trim();
	// Allow submit when input is empty but a context suggestion is selected
	const hasSelectedCompletion = completions.length > 0 && completionIndex >= 0;
	if ((!trimmed && !hasSelectedCompletion) || isExecuting || !backendReady) return;

	// If a plan is showing and user presses Enter, execute it
	if (pendingPlan) return;

	// Intercept built-in panel keywords before completions
	const lower = trimmed.toLowerCase();
	if (lower === "settings") {
		inputValue = "";
		settingsOpen = true;
		historyOpen = false;
		completions = [];
		completionIndex = -1;
		return;
	}

	if (lower === "history") {
		inputValue = "";
		historyOpen = true;
		settingsOpen = false;
		notesOpen = false;
		mediaOpen = false;
		completions = [];
		completionIndex = -1;
		return;
	}

	if (lower === "spotify" || lower === "media" || lower === "music") {
		inputValue = "";
		mediaOpen = true;
		historyOpen = false;
		completions = [];
		completionIndex = -1;
		return;
	}

	if (
		lower === "note" ||
		lower === "notes" ||
		lower === "todo" ||
		lower === "todos" ||
		lower === "reminder" ||
		lower === "reminders" ||
		lower === "timer" ||
		lower === "timers" ||
		lower === "stopwatch" ||
		lower === "snip" ||
		lower === "snippet" ||
		lower === "snippets"
	) {
		inputValue = "";
		notesOpen = true;
		historyOpen = false;
		completions = [];
		completionIndex = -1;
		if (lower === "reminder" || lower === "reminders") {
			initialNotesTab = "reminders";
		} else if (lower === "timer" || lower === "timers" || lower === "stopwatch") {
			initialNotesTab = "timers";
		} else if (lower === "todo" || lower === "todos") {
			initialNotesTab = "todos";
		} else if (lower === "snip" || lower === "snippet" || lower === "snippets") {
			initialNotesTab = "snippets";
		} else {
			initialNotesTab = "notes";
		}
		return;
	}

	// In search mode, auto-select first result if none explicitly selected
	if (searchMode && completions.length > 0 && completionIndex < 0) {
		completionIndex = 0;
	}

	// Colon triggers (e.g. "al:list", "tz:tokyo") — send directly to backend, skip completion selection
	if (/^[a-z]{1,4}:/.test(lower) && !lower.startsWith("http")) {
		await runCommand(trimmed);
		return;
	}

	// If completions are visible and one is selected, execute based on context
	if (completions.length > 0 && completionIndex >= 0) {
		const selected = completions[completionIndex];
		if (selected && selected.icon_path !== "__separator__") {
			// Calc results start with "= " — just display, don't execute
			if (selected.label.startsWith("= ")) {
				lastResult = {
					success: true,
					output: selected.label.slice(2),
					error: null,
					duration_ms: 0,
				};
				inputValue = "";
				completions = [];
				completionIndex = -1;
				return;
			}
			// "Did you mean: X?" typo suggestion — fill input with corrected text
			if (selected.label.startsWith("Did you mean:") && selected.description) {
				inputValue = selected.description;
				completions = [];
				completionIndex = -1;
				handleInput(inputValue);
				return;
			}
			// @ browse or / search mode — drill into directory or open file
			if (atMode || searchMode) {
				handleCompletionSelect(selected.label, opts?.ctrlKey);
				return;
			}
			// Check if input has an explicit prefix (e.g. "spotify ", "system ", "media ")
			// If so, append the selected completion to the prefix.
			// But if the first word isn't a known handler prefix, this is a natural
			// language query (e.g. "what is the weather here") — run it as-is so the
			// backend's keyword/AI routing handles it correctly.
			const KNOWN_PREFIXES = new Set([
				"ask",
				"bm",
				"bookmark",
				"browse",
				"clip",
				"clipboard",
				"close",
				"emoji",
				"focus",
				"kill",
				"open",
				"sym",
				"unicode",
				"web",
				"yt",
				"run",
				"calc",
				"file",
				"url",
				"media",
				"project",
				"quit",
				"system",
				"note",
				"notes",
				"todo",
				"todos",
				"weather",
				"sysinfo",
				"ip",
				"cpu",
				"mem",
				"disk",
				"temp",
				"gpu",
				"battery",
				"net",
				"audio",
				"display",
				"os",
				"speedtest",
				"time",
				"tz",
				"clock",
				"alias",
				"aliases",
				"timer",
				"stopwatch",
			]);
			const spaceIdx = trimmed.indexOf(" ");
			if (spaceIdx !== -1) {
				const prefix = trimmed.slice(0, spaceIdx).toLowerCase();
				if (KNOWN_PREFIXES.has(prefix)) {
					await runCommand(`${prefix} ${selected.label}`);
				} else {
					// Natural language — let the backend route the original input
					await runCommand(trimmed);
				}
			} else if (KNOWN_PREFIXES.has(lower)) {
				// Input is a bare prefix (e.g. "clip", "focus") — send prefix + selected label
				await runCommand(`${lower} ${selected.label}`);
			} else if (selected.icon_path === "__context__") {
				// Context suggestion — label is a complete command (e.g. "git commit", "run cargo build")
				await runCommand(selected.label);
			} else if (selected.label.toLowerCase() === lower) {
				// Completion matches input exactly (e.g. "mem" → sysinfo "mem")
				// Let the backend router handle it directly
				await runCommand(trimmed);
			} else {
				// No prefix — these are app completions, launch via open
				await runCommand(`open ${selected.label}`);
			}
			return;
		}
	}

	// In search mode, don't fall through to command execution
	if (searchMode) return;

	// C1/C15: Race AI plan against a short timeout.
	// If AI responds within 200ms, use the plan. Otherwise, execute immediately.
	// ESC cancels via generation counter — stale responses are discarded.
	const gen = ++routingGeneration;
	isRouting = true;

	const planPromise = getAgentPlan(trimmed).catch((err) => {
		console.error("[agent plan] error:", err);
		return null;
	});
	const timeout = new Promise<null>((r) => setTimeout(() => r(null), 200));

	const fastPlan = await Promise.race([planPromise, timeout]);

	// If ESC was pressed during the race, bail out
	if (gen !== routingGeneration) {
		isRouting = false;
		return;
	}

	if (fastPlan) {
		// AI responded quickly with a plan — show preview
		isRouting = false;
		pendingPlan = fastPlan;
		completions = [];
		completionIndex = -1;
		historyOpen = false;
		return;
	}

	// AI didn't respond in time — execute immediately, but let plan arrive in background
	isRouting = false;
	const commandPromise = runCommand(trimmed);

	// If AI plan arrives later and we're still on the same generation, offer it
	planPromise.then((plan) => {
		if (plan && gen === routingGeneration && !pendingPlan) {
			pendingPlan = plan;
		}
	});

	await commandPromise;
}

async function runCommand(command: string) {
	if (isExecuting) return;
	isExecuting = true;
	completions = [];
	completionIndex = -1;
	historyOpen = false;
	settingsOpen = false;
	mediaOpen = false;
	notesOpen = false;
	try {
		lastCommand = command;
		lastResult = await executeCommand(command);
		// If confirmation is needed, show the confirm panel and wait for user decision
		if (lastResult.needs_confirmation) {
			return;
		}
		historyEntries = [...historyEntries, command];
		inputValue = "";
		// If the backend wants us to open a URI, use GDK (proper Wayland focus transfer)
		// But if output is also present (e.g. "ask" handler), show inline result instead
		if (lastResult.open_url && !lastResult.output) {
			await hideWindow();
			await openUri(lastResult.open_url);
		} else if (lastResult.output === "__media_panel__") {
			mediaOpen = true;
			historyOpen = false;
			settingsOpen = false;
			notesOpen = false;

			lastResult = null;
		} else if (lastResult.output === "__notes_panel__") {
			notesOpen = true;
			historyOpen = false;
			settingsOpen = false;
			mediaOpen = false;

			lastResult = null;
		} else if (lastResult.output === "__timer_panel__") {
			notesOpen = true;
			initialNotesTab = "timers";
			historyOpen = false;
			settingsOpen = false;
			mediaOpen = false;
			lastResult = null;
		} else if (lastResult.output === "__reminders_panel__") {
			notesOpen = true;
			initialNotesTab = "reminders";
			historyOpen = false;
			settingsOpen = false;
			mediaOpen = false;

			lastResult = null;
		} else if (lastResult.output === "__snippets_panel__") {
			notesOpen = true;
			initialNotesTab = "snippets";
			historyOpen = false;
			settingsOpen = false;
			mediaOpen = false;

			lastResult = null;
		} else if (lastResult.output?.startsWith("__notes_limit__:")) {
			pendingNoteText = lastResult.output.slice("__notes_limit__:".length);
			notesOpen = true;
			historyOpen = false;
			settingsOpen = false;
			mediaOpen = false;

			lastResult = null;
		} else if (lastResult.output?.startsWith("__browse_panel__:")) {
			const dir = lastResult.output.slice("__browse_panel__:".length);
			inputValue = `@${dir}`;
			atMode = true;
			atStart = 0;
			lastResult = null;
			fetchAtCompletions(dir);
		} else if (lastResult.success && !lastResult.output) {
			await hideWindow();
		}
	} catch (err) {
		lastResult = {
			success: false,
			output: null,
			error: String(err),
			duration_ms: 0,
		};
	} finally {
		isExecuting = false;
	}
}

async function handleConfirm() {
	if (isExecuting || !lastResult?.needs_confirmation) return;
	isExecuting = true;
	try {
		// C6: If this is a privacy consent confirmation, grant and persist it
		const reason = lastResult.needs_confirmation;
		if (reason.includes("freeipapi.com")) {
			await grantPrivacyConsent("ip_geolocation");
		} else if (reason.includes("ifconfig.me")) {
			await grantPrivacyConsent("public_ip");
		}

		lastResult = await executeCommand(lastCommand, true);
		historyEntries = [...historyEntries, lastCommand];
		inputValue = "";
		if (lastResult.open_url && !lastResult.output) {
			await hideWindow();
			await openUri(lastResult.open_url);
		} else if (lastResult.success && !lastResult.output) {
			await hideWindow();
		}
	} catch (err) {
		lastResult = { success: false, output: null, error: String(err), duration_ms: 0 };
	} finally {
		isExecuting = false;
	}
}

function handleConfirmDismiss() {
	lastResult = null;
}

async function openFileByLabel(label: string) {
	searchMode = false;
	atMode = false;
	atStart = -1;
	completions = [];
	completionIndex = -1;
	cancelFileSearch();
	inputValue = "";
	await hideWindow();
	await openUri(`file://${label}`);
}

function handleCompletionSelect(label: string, forceOpen?: boolean) {
	// / search mode — drill into folders (Enter), or open in file manager (Ctrl+Enter)
	if (searchMode) {
		const item = completions[completionIndex];
		if (item?.icon_path === "__folder__" && !forceOpen) {
			// Make label relative to active scope for the input value
			let path = item.label;
			if (path.startsWith("~/")) {
				path = path.slice(2); // ~/Downloads/ → Downloads/
			} else if (activeScope && path.startsWith(activeScope)) {
				path = path.slice(activeScope.length); // /mnt/DevSSD/Colris/ → Colris/
				if (path.startsWith("/")) path = path.slice(1);
			}
			inputValue = `/${path}`;
			handleInput(inputValue);
		} else {
			openFileByLabel(label);
		}
		return;
	}

	// @ browse mode — reference a file in the command line
	if (atMode) {
		if (label.endsWith("/")) {
			const before = inputValue.slice(0, atStart);
			const afterAt = inputValue.slice(atStart);
			const spaceIdx = afterAt.indexOf(" ", 1); // skip the @ itself
			const after = spaceIdx === -1 ? "" : afterAt.slice(spaceIdx);
			// Set input and immediately fetch — handleInput will also fire but
			// debounce means our direct call wins
			inputValue = `${before}@${label}${after}`;
			clearTimeout(debounceTimer);
			fetchAtCompletions(label);
			requestAnimationFrame(() => {
				document.querySelector<HTMLInputElement>(".input-container input")?.focus();
			});
		} else {
			// Insert the file path into the command, replacing the @partial
			const before = inputValue.slice(0, atStart);
			const afterAt = inputValue.slice(atStart);
			const spaceIdx = afterAt.indexOf(" ", 1);
			const after = spaceIdx === -1 ? "" : afterAt.slice(spaceIdx);
			inputValue = `${before}@${label}${after}`;
			atMode = false;
			atStart = -1;
			completions = [];
			completionIndex = -1;
		}
		return;
	}

	// Find the item by label (click passes label directly, may not match completionIndex)
	const item = completions.find((c) => c.label === label) ?? completions[completionIndex];

	// "Did you mean: X?" typo suggestion — fill input with corrected text
	if (item?.description && label.startsWith("Did you mean:")) {
		inputValue = item.description;
		handleInput(inputValue);
		return;
	}

	// Context suggestion — label is a complete command (e.g. "git commit", "run cargo build")
	if (item?.icon_path === "__context__") {
		runCommand(label);
		return;
	}

	runCommand(`open ${label}`);
}

function handleScopeChange(index: number) {
	scopeIndex = index;
	if (searchMode && inputValue.startsWith("/")) {
		// Re-trigger search with new scope
		searchDone = false;
		fileSearchId++;
		const id = fileSearchId;
		completions = [];
		completionIndex = -1;
		searchScopePath = "";

		const raw = inputValue.slice(1);
		const lastSlash = raw.lastIndexOf("/");
		const baseScope = mountPoints[index]?.path ?? "";

		let searchScope: string;
		let searchTerm: string;
		if (lastSlash >= 0) {
			const folderPart = raw.slice(0, lastSlash);
			searchTerm = raw.slice(lastSlash + 1);
			searchScope = folderPart ? `${baseScope}/${folderPart}` : baseScope;
			searchScopePath = searchScope;
		} else {
			searchTerm = raw;
			searchScope = baseScope;
		}

		if (searchScope) startFileSearch(searchTerm, searchScope, id);
	}
}

function handleHistorySelect(entry: string) {
	inputValue = entry;
	historyOpen = false;
}

function handleToggleHistory() {
	historyOpen = !historyOpen;
	settingsOpen = false;
	mediaOpen = false;
	notesOpen = false;
	if (historyOpen) {
		completions = [];
		completionIndex = -1;
	} else {
		// Re-fetch completions for current input
		handleInput(inputValue);
	}
}

function handleToggleSettings() {
	settingsOpen = !settingsOpen;
	if (settingsOpen) {
		historyOpen = false;
		mediaOpen = false;
		notesOpen = false;

		completions = [];
		completionIndex = -1;
	}
}

function handleToggleMedia() {
	mediaOpen = !mediaOpen;
	if (mediaOpen) {
		historyOpen = false;
		settingsOpen = false;
		notesOpen = false;

		completions = [];
		completionIndex = -1;
	}
}

function handleToggleNotes() {
	notesOpen = !notesOpen;
	if (notesOpen) {
		historyOpen = false;
		settingsOpen = false;
		mediaOpen = false;

		completions = [];
		completionIndex = -1;
	}
}

function handleShowResult() {
	settingsOpen = false;
	mediaOpen = false;
	historyOpen = false;
	notesOpen = false;
	completions = [];
	completionIndex = -1;
}

function handleShowPlan() {
	settingsOpen = false;
	mediaOpen = false;
	historyOpen = false;
	notesOpen = false;
	completions = [];
	completionIndex = -1;
}

function handleArrowUp() {
	if (completions.length > 0) {
		let next = completionIndex - 1;
		// Skip separator items
		while (next >= 0 && completions[next]?.icon_path === "__separator__") next--;
		completionIndex = Math.max(0, next);
	}
}

function handleArrowDown() {
	if (completions.length > 0) {
		let next = completionIndex + 1;
		// Skip separator items
		while (next < completions.length && completions[next]?.icon_path === "__separator__") next++;
		completionIndex = Math.min(completions.length - 1, next);
	}
}

async function handleDismiss() {
	// C15: Cancel in-flight AI routing immediately on ESC
	if (isRouting) {
		routingGeneration++;
		isRouting = false;
	}
	await hideWindow();
}
</script>

<div class="launcher-wrapper" class:layer-shell={windowStrategy === 'layer-shell'} role="presentation" onmousedown={(e) => { if (windowStrategy !== 'layer-shell' && e.target === e.currentTarget) hideWindow(); }}>
	<div class="launcher-row">
	<main>
		<CommandInput
			bind:value={inputValue}
			onsubmit={handleSubmit}
			onarrowup={handleArrowUp}
			onarrowdown={handleArrowDown}
			ondismiss={handleDismiss}
			oninputchange={handleInput}
			ontogglehistory={handleToggleHistory}
			ontogglemedia={handleToggleMedia}
			ontogglesettings={handleToggleSettings}
			ontogglenotes={handleToggleNotes}
			disabled={isExecuting || isRouting}
			routing={isRouting}
			executing={isExecuting}
			{contextPill}
			{atMode}
			{atStart}
			{searchMode}
			scopeCount={mountPoints.length}
			ontabscope={() => handleScopeChange((scopeIndex + 1) % mountPoints.length)}
			ontabcomplete={() => {
				if (completions.length > 0 && completionIndex >= 0) {
					const item = completions[completionIndex];
					if (searchMode && item.icon_path === "__folder__") {
						// Drill into folder — make label relative to active scope
						let path = item.label;
						if (path.startsWith("~/")) {
							path = path.slice(2);
						} else if (activeScope && path.startsWith(activeScope)) {
							path = path.slice(activeScope.length);
							if (path.startsWith("/")) path = path.slice(1);
						}
						inputValue = `/${path}`;
						handleInput(inputValue);
					} else if (searchMode) {
						// File in search mode — open it
						openFileByLabel(item.label);
					} else {
						// Browse mode — existing behavior
						handleCompletionSelect(item.label);
					}
				}
			}}
			onshifttabback={() => {
				if (searchMode && inputValue.startsWith("/")) {
					const raw = inputValue.slice(1);
					// Find last slash, then the one before it to go up
					const trimmed = raw.endsWith("/") ? raw.slice(0, -1) : raw;
					const lastSlash = trimmed.lastIndexOf("/");
					if (lastSlash > 0) {
						inputValue = `/${trimmed.slice(0, lastSlash + 1)}`;
					} else {
						inputValue = "/";
					}
					handleInput(inputValue);
				} else if (atMode && atStart >= 0) {
					const partial = inputValue.slice(atStart + 1);
					const afterAt = inputValue.slice(atStart);
					const spaceIdx = afterAt.indexOf(" ", 1);
					const after = spaceIdx === -1 ? "" : afterAt.slice(spaceIdx);
					const before = inputValue.slice(0, atStart);

					// Strip trailing slash, find parent
					const trimmed = partial.endsWith("/") ? partial.slice(0, -1) : partial;
					const lastSlash = trimmed.lastIndexOf("/");
					if (lastSlash > 0) {
						const parent = trimmed.slice(0, lastSlash + 1);
						inputValue = `${before}@${parent}${after}`;
						clearTimeout(debounceTimer);
						fetchAtCompletions(parent);
					} else {
						inputValue = `${before}@${after}`;
						clearTimeout(debounceTimer);
						fetchAtCompletions("");
					}
				}
			}}
			searchGhost={searchMode && completions.length > 0 && completionIndex >= 0 ? completions[completionIndex].label : ""}
			browseGhost={atMode && completions.length > 0 && completionIndex >= 0 ? completions[completionIndex].label : ""}
			history={historyEntries}
		/>
		<!-- Panels: always mounted, hidden via CSS (visibility:hidden) for instant toggle -->
		<!-- Order matches shortcuts: Ctrl+1 History, Ctrl+2 Notes, Ctrl+3 Media, Ctrl+4 Settings -->
		<div class:panel-hidden={!historyOpen}>
			<HistoryPanel entries={historyEntries} onselect={handleHistorySelect} />
		</div>
		<div class:panel-hidden={!notesOpen}>
			<NotesPanel ondismiss={() => { notesOpen = false; pendingNoteText = null; initialNotesTab = undefined; }} {pendingNoteText} onpendingcleared={() => { pendingNoteText = null; }} {initialNotesTab} visible={notesOpen} />
		</div>
		<div class:panel-hidden={!mediaOpen}>
			<MediaPanel ondismiss={() => { mediaOpen = false; }} players={mediaPlayers} />
		</div>
		<div class:panel-hidden={!settingsOpen}>
			<SettingsPanel ondismiss={() => { settingsOpen = false; }} />
		</div>
		{#if pendingPlan}
			<AgentPlanPanel
				bind:this={planPanelRef}
				plan={pendingPlan}
				onexecuted={() => {
					historyEntries = [...historyEntries, pendingPlan!.input];
					inputValue = "";
				}}
				ondismiss={() => { pendingPlan = null; }}
			/>
		{:else if !settingsOpen && !notesOpen && !mediaOpen && !historyOpen}
			<CompletionsList
				items={completions}
				selectedIndex={completionIndex}
				onselect={handleCompletionSelect}
				pathContext={searchMode ? searchPathContext : (atMode ? atPathContext : "")}
				scopeTabs={searchMode && mountPoints.length > 1 ? mountPoints : []}
				activeScopeIndex={scopeIndex}
				onscopechange={handleScopeChange}
				searching={searchMode && !searchDone}
				browseMode={atMode}
				searchMode={searchMode}
				metaMap={searchMode ? fileMetaMap : undefined}
				ignoreActive={searchMode && ignoreActive}
			/>
			{#if completions.length === 0 && atMode && atNoResults}
				<div class="empty-state">Empty folder</div>
			{:else if completions.length === 0 && searchMode && !searchDone}
				<div class="empty-state">Searching...</div>
			{/if}
			{#if lastResult}
				<ResultPanel result={lastResult} command={lastCommand}
				onconfirm={handleConfirm} ondismiss={handleConfirmDismiss}
				onopenurl={async () => {
					if (lastResult?.open_url) {
						await hideWindow();
						await openUri(lastResult.open_url);
					}
				}}
				onopenfile={(path) => runCommand(`file ${path}`)} />
			{/if}
		{/if}
		<StatusBar
			result={lastResult}
			executing={isExecuting}
			routing={isRouting}
			{historyOpen}
			{settingsOpen}
			mediaOpen={mediaOpen}
			{nowPlaying}
			{multiplePlayers}
			ontogglehistory={handleToggleHistory}
			ontogglesettings={handleToggleSettings}
			ontogglemedia={handleToggleMedia}
			ontogglenotes={handleToggleNotes}
			{notesOpen}
		onshowresult={handleShowResult}
		onshowplan={handleShowPlan}
		hasPlan={!!pendingPlan}
		/>
	</main>
	{#if showPreview}
		<FilePreview filePath={previewPath} visible={showPreview} />
	{/if}
	</div>
</div>

<style>
	.launcher-wrapper {
		width: 100%;
		height: 100%;
		display: flex;
		justify-content: center;
		padding-top: 18vh;
		background: transparent;
	}

	/* Layer-shell: Rust sets surface width adaptively (1070px wide or 680px narrow).
	   Use 100vw to fill whatever surface Rust provides. */
	.launcher-wrapper.layer-shell {
		width: 100vw;
		height: auto;
		padding-top: 0;
		padding-bottom: 40px; /* room for box-shadow below the bar */
		justify-content: flex-start; /* left-align so preview has room to the right */
	}

	/* Surface is already 60% of monitor height, so use full viewport */
	.launcher-wrapper.layer-shell main {
		max-height: calc(100vh - 40px);
	}

	.launcher-row {
		position: relative;
		width: 680px;
		max-width: calc(100vw - 40px);
		align-self: flex-start;
	}

	main {
		width: 680px;
		max-width: calc(100vw - 40px);
		max-height: 60vh;
		display: flex;
		flex-direction: column;
		background: var(--bg);
		border: 1px solid var(--border);
		border-radius: 12px;
		overflow: hidden;
		box-shadow: 0 8px 32px rgba(0, 0, 0, 0.5);
		animation: lychi-appear 120ms ease-out;
	}

	.panel-hidden {
		visibility: hidden;
		position: absolute;
		pointer-events: none;
		width: 100%;
		will-change: transform;
	}

	.empty-state {
		padding: 12px 20px;
		font-family: var(--font-mono);
		font-size: 13px;
		color: var(--fg-muted);
		opacity: 0.6;
	}

	:global(main.lychi-closing) {
		animation: lychi-disappear 100ms ease-in forwards;
	}

	@keyframes lychi-appear {
		from {
			opacity: 0;
			transform: translateY(-8px);
		}
		to {
			opacity: 1;
			transform: translateY(0);
		}
	}

	@keyframes lychi-disappear {
		from {
			opacity: 1;
			transform: translateY(0);
		}
		to {
			opacity: 0;
			transform: translateY(-8px);
		}
	}
</style>

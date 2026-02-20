<script lang="ts">
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
	FileSearchBatch,
	MountPoint,
	StepEvent,
	TrackInfo,
} from "$lib/ipc";
import {
	cancelFileSearch,
	executeCommand,
	getAgentPlan,
	getCompletions,
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
import { loadKeybindings } from "$lib/keybindings";
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

let pendingPlan: AgentPlan | null = $state(null);
let planPanelRef: AgentPlanPanel | undefined = $state(undefined);
// C1/C15: generation counter for AI routing — ESC increments to cancel stale responses
let routingGeneration = 0;
let mediaOpen = $state(false);
let notesOpen = $state(false);
let pendingNoteText: string | null = $state(null);
let mediaPlayers: TrackInfo[] = $state([]);
let mediaPollTimer: ReturnType<typeof setTimeout> | undefined;

// Derive the "now playing" track — first playing, or first in list
let nowPlaying = $derived.by(() => {
	const playing = mediaPlayers.find((p) => p.status === "playing");
	return playing ?? mediaPlayers[0] ?? null;
});

let multiplePlayers = $derived(mediaPlayers.length > 1);

// File preview — show for non-folder files in search or browse mode
let previewPath = $derived.by(() => {
	if (!searchMode && !atMode) return "";
	if (completions.length === 0 || completionIndex < 0) return "";
	const item = completions[completionIndex];
	if (!item || item.icon_path === "__folder__") return "";
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
	historyOpen = false;
	clearTimeout(debounceTimer);
	atNoResults = false;

	// Detect / file search mode — must be at the start of input
	if (val.startsWith("/")) {
		const raw = val.slice(1);
		if (!raw.includes(" ")) {
			// Exit @ mode if active
			atMode = false;
			atStart = -1;

			searchMode = true;
			searchDone = false;
			fileSearchId++;
			const id = fileSearchId;

			// Parse path: /folder/subfolder/query → scope=folder/subfolder, searchTerm=query
			const lastSlash = raw.lastIndexOf("/");
			let searchScope: string;
			let searchTerm: string;
			if (lastSlash >= 0) {
				const folderPart = raw.slice(0, lastSlash); // e.g. "Documents/reports"
				searchTerm = raw.slice(lastSlash + 1); // e.g. "q1" or ""
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
				completions = [];
				completionIndex = -1;
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
	if (trimmed.length < 2) {
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

				// Populate path map for preview
				const pathUpdates = new Map(filePathMap);
				for (const r of batch.results) {
					pathUpdates.set(r.label, r.full_path);
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

		// Hide on blur
		const unlistenFocus = await win.onFocusChanged(({ payload: focused }) => {
			if (!focused && hideOnBlur && blurEnabled && !settingsOpen && !notesOpen) {
				hideWindow();
			}
		});
		unlisteners.push(unlistenFocus);

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

async function handleSubmit() {
	const trimmed = inputValue.trim();
	if (!trimmed || isExecuting || !backendReady) return;

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

	if (lower === "spotify" || lower === "media" || lower === "music") {
		inputValue = "";
		mediaOpen = true;
		historyOpen = false;
		completions = [];
		completionIndex = -1;
		return;
	}

	if (lower === "notes" || lower === "todo" || lower === "todos") {
		inputValue = "";
		notesOpen = true;
		historyOpen = false;
		completions = [];
		completionIndex = -1;
		return;
	}

	// In search mode, auto-select first result if none explicitly selected
	if (searchMode && completions.length > 0 && completionIndex < 0) {
		completionIndex = 0;
	}

	// If completions are visible and one is selected, execute based on context
	if (completions.length > 0 && completionIndex >= 0) {
		const selected = completions[completionIndex];
		if (selected) {
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
			// @ browse or / search mode — drill into directory or open file
			if (atMode || searchMode) {
				handleCompletionSelect(selected.label);
				return;
			}
			// Check if input has an explicit prefix (e.g. "spotify ", "system ", "media ")
			// If so, append the selected completion to the prefix
			const spaceIdx = trimmed.indexOf(" ");
			if (spaceIdx !== -1) {
				const prefix = trimmed.slice(0, spaceIdx);
				await runCommand(`${prefix} ${selected.label}`);
			} else if (selected.label.toLowerCase() === trimmed.toLowerCase()) {
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

function handleCompletionSelect(label: string) {
	// / search mode — find and open files/folders directly
	if (searchMode) {
		openFileByLabel(label);
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

	runCommand(`open ${label}`);
}

function handleScopeChange(index: number) {
	scopeIndex = index;
	if (searchMode && inputValue.startsWith("/")) {
		// Re-trigger search with new scope — keep old results visible until new arrive
		const query = inputValue.slice(1);
		if (query) {
			searchDone = false;
			fileSearchId++;
			const id = fileSearchId;
			completions = [];
			completionIndex = -1;
			startFileSearch(query, mountPoints[index]?.path ?? "", id);
		}
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
		completionIndex = Math.max(0, completionIndex - 1);
	}
}

function handleArrowDown() {
	if (completions.length > 0) {
		completionIndex = Math.min(completions.length - 1, completionIndex + 1);
	}
}

async function handleDismiss() {
	// C15: Cancel in-flight AI routing immediately on ESC
	if (isRouting) {
		routingGeneration++;
		isRouting = false;
		return;
	}
	if (settingsOpen) {
		settingsOpen = false;
		return;
	}
	if (mediaOpen) {
		mediaOpen = false;
		return;
	}
	if (notesOpen) {
		notesOpen = false;
		return;
	}
	if (lastResult?.needs_confirmation) {
		lastResult = null;
		return;
	}
	if (pendingPlan) {
		pendingPlan = null;
		return;
	}
	if (completions.length > 0 || searchMode) {
		completions = [];
		completionIndex = -1;
		atMode = false;
		atStart = -1;
		if (searchMode) {
			cancelFileSearch();
			searchMode = false;
		}
		return;
	}
	if (historyOpen) {
		historyOpen = false;
		return;
	}
	await hideWindow();
}
</script>

<div class="launcher-wrapper" role="presentation" onmousedown={(e) => { if (e.target === e.currentTarget) hideWindow(); }}>
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
			{atMode}
			{atStart}
			{searchMode}
			scopeCount={mountPoints.length}
			ontabscope={() => handleScopeChange((scopeIndex + 1) % mountPoints.length)}
			ontabcomplete={() => {
				if (completions.length > 0 && completionIndex >= 0) {
					const item = completions[completionIndex];
					if (searchMode && item.icon_path === "__folder__") {
						// Drill into folder — set input to folder path
						const path = item.label.startsWith("~/") ? item.label.slice(2) : item.label;
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
			<NotesPanel ondismiss={() => { notesOpen = false; pendingNoteText = null; }} {pendingNoteText} onpendingcleared={() => { pendingNoteText = null; }} />
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

	.launcher-row {
		position: relative;
		width: 680px;
		align-self: flex-start;
	}

	main {
		width: 680px;
		max-height: 60vh;
		display: flex;
		flex-direction: column;
		background: var(--bg);
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

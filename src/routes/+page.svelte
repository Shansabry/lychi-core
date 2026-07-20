<script lang="ts">
import { invoke } from "@tauri-apps/api/core";
import { LogicalSize } from "@tauri-apps/api/dpi";
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
	getAiStatus,
	getCompletions,
	getContext,
	getHideOnBlur,
	getHistory,
	getHotkeyStatus,
	getMountPoints,
	grantPrivacyConsent,
	hideWindow,
	listPathCompletions,
	mediaGetStatus,
	openPath,
	openUri,
	revealPath,
	saveGeneralConfig,
	saveWindowPosition,
	startFileSearch,
} from "$lib/ipc";
import { loadKeybindings, matchesAction } from "$lib/keybindings";
import { preloadAll } from "$lib/preloadCache";

let inputValue = $state("");
let isExecuting = $state(false);
// Bumped when a stuck/in-flight command is cancelled via Escape, so a late
// resolution of the abandoned executeCommand promise is ignored instead of
// clobbering fresh state.
let executeGeneration = 0;
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
// True while a completions query is in flight. Drives a subtle skeleton in the
// results area for the narrow window where the first query (cold start) hasn't
// returned yet — non-blocking, vanishes the instant results arrive. The launcher
// stays fully typeable throughout; this is a hint, not a loading gate.
let completionsPending = $state(false);

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
let windowStrategy = $state("x11"); // "layer-shell" or "x11" — drives layout mode
// Non-composited X11 (xfwm4/Marco with compositing off): the window is a
// compact opaque rofi-style box sized to content — transparency would render
// black. Drives the .compact/.no-compositor layout and dynamic window resize.
let compactMode = $state(false);
let launcherRowEl: HTMLDivElement | undefined = $state();

// Keep the compact window's height matched to content (rofi-style resize).
$effect(() => {
	if (!compactMode || !launcherRowEl || !("__TAURI_INTERNALS__" in window)) return;
	const el = launcherRowEl;
	const win = getCurrentWindow();
	let lastH = 0;
	const observer = new ResizeObserver(() => {
		const maxH = Math.round(window.screen.height * 0.6);
		const h = Math.min(Math.ceil(el.getBoundingClientRect().height), maxH);
		if (h > 0 && h !== lastH) {
			lastH = h;
			win.setSize(new LogicalSize(680, h)).catch(() => {});
		}
	});
	observer.observe(el);
	return () => observer.disconnect();
});
// First-run Wayland onboarding: shown when the in-app hotkey can't work
// system-wide and the user hasn't dismissed the tip yet.
let hotkeyBannerVisible = $state(false);
let hotkeyBannerCopied = $state(false);
let loadedGeneralConfig: import("$lib/ipc").GeneralConfig | null = null;

function dismissHotkeyBanner() {
	hotkeyBannerVisible = false;
	if (loadedGeneralConfig && !loadedGeneralConfig.first_run_completed) {
		loadedGeneralConfig.first_run_completed = true;
		saveGeneralConfig(loadedGeneralConfig).catch((err) => {
			console.error("[onboarding] failed to persist banner dismissal:", err);
		});
	}
}

async function copyToggleCommand() {
	try {
		await navigator.clipboard.writeText("lychi --toggle");
		hotkeyBannerCopied = true;
		setTimeout(() => {
			hotkeyBannerCopied = false;
		}, 1500);
	} catch (err) {
		console.error("[onboarding] clipboard write failed:", err);
	}
}
// Prevents stale-completions flash: kept false until summon clears state, so the
// first compositor frame is always a clean empty launcher.
let launcherReady = $state(false);

// Context-staleness indicator (shown as a dim glyph in the status bar, not a
// warning row). Set from a `__context_stale__` sentinel in the completions.
let contextStale = $state(false);
let contextStaleHint = $state("");
// True only while a background context re-gather is actually in flight (between
// the `context-stale` and `context-ready` events) — drives the "updating
// context…" spinner, distinct from the idle "context outdated" bulb.
let contextRefreshing = $state(false);

/**
 * Pull a `__context_stale__` sentinel out of zero-state completions: sets the
 * status-bar indicator and returns the list WITHOUT it, so staleness shows as a
 * quiet glyph instead of an invasive warning row. Returns results unchanged
 * when the sentinel is absent (and clears the indicator).
 */
function extractContextStale(results: CompletionItem[]): CompletionItem[] {
	const stale = results.find((c) => c.icon_path === "__context_stale__");
	contextStale = !!stale;
	contextStaleHint = stale?.description ?? "";
	return stale ? results.filter((c) => c.icon_path !== "__context_stale__") : results;
}

// Transient confirmation shown in the completions hints bar (e.g. "Path copied").
let flashMessage = $state("");
let flashTimer: ReturnType<typeof setTimeout> | undefined;
function flashHint(msg: string) {
	flashMessage = msg;
	clearTimeout(flashTimer);
	flashTimer = setTimeout(() => {
		flashMessage = "";
	}, 1400);
}

let pendingPlan: AgentPlan | null = $state(null);
let planPanelRef: AgentPlanPanel | undefined = $state(undefined);
let resultPanelRef: ResultPanel | undefined = $state(undefined);
// C1/C15: generation counter for AI routing — ESC increments to cancel stale responses
let routingGeneration = 0;
let mediaOpen = $state(false);
let notesOpen = $state(false);
let pendingNoteText: string | null = $state(null);
let initialNotesTab: "notes" | "todos" | "reminders" | "timers" | "snippets" | undefined =
	$state(undefined);
let mediaPlayers: TrackInfo[] = $state([]);
// Whether an AI provider is actually configured — gates the natural-language
// placeholder suggestions so we never advertise AI-only actions to a user who
// has no key (they'd silently web-search instead).
let aiEnabled = $state(false);
let envContext: EnvironmentContext | null = $state(null);
let contextLoading = $state(false);
let contextLoadingTimer: ReturnType<typeof setTimeout> | undefined;
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
		completionsPending = false;
		completions = [];
		completionIndex = -1;
		return;
	}

	const gen = ++completionGen;
	completionsPending = true;
	getCompletions(trimmed)
		.then((rawResults) => {
			if (gen !== completionGen) return;
			completionsPending = false;
			const results = extractContextStale(rawResults);
			completions = results;
			// Omnibox rule (Chromium `allowed_to_be_default_match`): only
			// auto-select a suggestion whose text is a prefix-extension of what
			// the user literally typed. Otherwise select nothing, so plain Enter
			// runs the typed input — never a non-prefix match. e.g. "run top"
			// must NOT auto-run the history entry "run htop".
			completionIndex = defaultMatchIndex(results, trimmed);
		})
		.catch((err) => {
			if (gen === completionGen) completionsPending = false;
			console.error("[completions] error:", err);
		});
}

/**
 * Index of the completion eligible to be Enter's default, or -1 if none.
 * A candidate qualifies only when it forward-completes the typed input —
 * its command (`run`/`fill`) or label starts with the input, case-insensitively.
 * Separators and info rows are never eligible. This is the browser-omnibox
 * model: with no explicit arrow-selection, Enter runs the typed input unless a
 * true prefix-completion exists.
 */
function defaultMatchIndex(results: CompletionItem[], input: string): number {
	const q = input.trim().toLowerCase();
	if (!q) return results.length > 0 ? 0 : -1;
	for (let i = 0; i < results.length; i++) {
		const c = results[i];
		if (c.icon_path === "__separator__" || c.icon_path === "__info__") continue;
		// The text this row would act on — prefer the exact command it carries.
		const candidate = (c.run ?? c.fill ?? c.label).toLowerCase();
		if (candidate.startsWith(q)) return i;
	}
	return -1;
}

onMount(() => {
	getActiveWindowStrategy().then((s) => {
		windowStrategy = s;
	});
	getHideOnBlur().then((v) => {
		hideOnBlur = v;
	});
	getAiStatus().then((s) => {
		aiEnabled = s.has_ai_router;
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
			loadedGeneralConfig = settings.generalConfig;
			compactMode = settings.activeWindowStrategy === "x11" && !settings.screenComposited;
			// First-run onboarding: on Wayland the in-app hotkey only fires over
			// XWayland windows — point the user at a DE-bound `lychi --toggle`.
			if (!settings.generalConfig.first_run_completed) {
				getHotkeyStatus()
					.then((status) => {
						if (status.session_type === "wayland" && !status.reliable) {
							hotkeyBannerVisible = true;
						} else {
							// Hotkey works — mark onboarding done silently
							dismissHotkeyBanner();
						}
					})
					.catch(() => {});
			}
		})
		.finally(() => {
			backendReady = true;
			if (inputValue.trim()) handleInput(inputValue);
		});

	// Guard: only attach Tauri listeners if running inside Tauri
	if (!("__TAURI_INTERNALS__" in window)) return;

	let unlisteners: (() => void)[] = [];

	(async () => {
		const win = getCurrentWindow();

		// Self-heal: if the window was mapped before these listeners attached
		// (cold start — the summon/shown events fired into the void), become
		// visible/clickable now instead of staying an invisible click-blocker.
		win
			.isVisible()
			.then((visible) => {
				if (visible) launcherReady = true;
			})
			.catch(() => {});

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

		// Ready signal — emitted post-map (with a 150ms watchdog re-emit).
		// Idempotent: only flips visibility readiness. Without it, a lost
		// summon leaves an invisible surface that blocks all desktop clicks.
		const unlistenShown = await win.listen("lychi://shown", () => {
			launcherReady = true;
		});
		unlisteners.push(unlistenShown);

		// Listen for summon event from Rust (global shortcut / IPC toggle)
		const unlistenSummon = await win.listen("lychi://summon", () => {
			launcherReady = true;
			inputValue = "";
			lastResult = null;
			historyOpen = false;
			settingsOpen = false;
			pendingPlan = null;
			mediaOpen = false;
			notesOpen = false;
			// Clear stale context immediately — fast path re-populates if same window,
			// fresh gather populates for changed windows. Prevents flash of wrong context.
			envContext = null;
			// Delayed spinner: only show skeleton if context takes >120ms
			clearTimeout(contextLoadingTimer);
			contextLoading = false;
			contextLoadingTimer = setTimeout(() => {
				contextLoading = true;
			}, 120);

			completions = [];
			completionIndex = -1;
			atMode = false;
			atStart = -1;
			searchMode = false;
			// C15: Cancel any in-flight AI routing on summon reset
			routingGeneration++;
			isRouting = false;
			cancelFileSearch();
			// Force focus the input (layer shell may not auto-focus DOM elements).
			// Double-tap: rAF for immediate attempt, setTimeout for delayed retry
			// in case the compositor grants surface focus slightly late.
			requestAnimationFrame(() => {
				document.querySelector<HTMLInputElement>(".input-container input")?.focus();
			});
			setTimeout(() => {
				document.querySelector<HTMLInputElement>(".input-container input")?.focus();
			}, 50);
		});
		unlisteners.push(unlistenSummon);

		// A background context re-gather has STARTED (fired by execute_command
		// when it runs against stale context) — show "updating context…".
		const unlistenContextStale = await win.listen("lychi://context-stale", () => {
			contextRefreshing = true;
		});
		unlisteners.push(unlistenContextStale);

		// Listen for context-ready event from async context gathering
		const unlistenContext = await win.listen<EnvironmentContext>("lychi://context-ready", (e) => {
			envContext = e.payload;
			contextRefreshing = false;
			clearTimeout(contextLoadingTimer);
			contextLoading = false;
			// Fetch context suggestions only if the input is still empty.
			// Use Svelte state (inputValue) not a DOM query — DOM can be stale when
			// focus hasn't been granted yet. Guard with completionGen so a fast typist
			// doesn't get their completions overwritten by the context response.
			if (inputValue.trim().length < 1) {
				const gen = ++completionGen;
				getCompletions("")
					.then((rawResults) => {
						if (gen !== completionGen) return;
						const results = extractContextStale(rawResults);
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

				// A section-header row (backend tags it with description "__separator__").
				// Rendered by CompletionsList as a centered label between lines; not
				// selectable. Everything else is a normal file/folder result.
				const newItems: CompletionItem[] = batch.results.map((r) => {
					const isSep = r.description === "__separator__";
					return {
						label: r.label,
						icon_path: isSep ? "__separator__" : r.is_dir ? "__folder__" : null,
						score: r.score,
						description: isSep ? null : (r.description ?? null),
					};
				});

				const isDone = batch.done;

				// Defer state update to next frame so it never blocks a keystroke paint
				requestAnimationFrame(() => {
					filePathMap = pathUpdates;
					fileMetaMap = metaUpdates;
					if (batch.has_ignore_rules) ignoreActive = true;

					// Each batch carries the FULL ranked snapshot for this search (the
					// index re-emits the whole top-N as it fills), already ordered by
					// the backend — fzf/Raycast standard: the backend owns ranking, the
					// UI just renders. REPLACE, don't append (appending re-adds the
					// same paths every emit → duplicates). Dedup by full_path as a
					// guard; preserve the backend's order (no re-sort). Section headers
					// (empty full_path) are kept verbatim — never deduped away.
					const seen = new Set<string>();
					completions = newItems
						.filter((item) => {
							if (item.icon_path === "__separator__") return true;
							const key = pathUpdates.get(item.label) ?? item.label;
							if (seen.has(key)) return false;
							seen.add(key);
							return true;
						})
						.slice(0, 20);

					// Auto-select the first SELECTABLE row (skip a leading section header).
					if (completionIndex < 0) {
						const first = completions.findIndex((c) => c.icon_path !== "__separator__");
						if (first >= 0) completionIndex = first;
					}

					if (isDone) {
						searchDone = true;
						atNoResults = completions.length === 0;
					}
				});
			},
		);
		unlisteners.push(unlistenFileSearch);

		// Window-level key handling (capture phase) — needed for actions that
		// must work even when DOM focus has left the input (e.g. after a
		// command runs and the result panel is showing).
		window.addEventListener(
			"keydown",
			(e) => {
				// Prevent WebKitGTK from stealing tab_back for native focus nav
				if (matchesAction(e, "tab_back")) {
					e.preventDefault();
					return;
				}
				// Browse the current result's inline URL via the user-assigned
				// open_inline_url binding — focus-independent so it fires from
				// the result panel too.
				if (lastResult?.open_url && matchesAction(e, "open_inline_url")) {
					e.preventDefault();
					openInlineUrl();
				}
				// Copy a shown QR image with the copy_path shortcut (Ctrl+Shift+C).
				// Only when a QR result is visible and not in file-search mode
				// (there the same binding copies the selected path — no conflict).
				if (!searchMode && lastResult?.output_type === "svg" && matchesAction(e, "copy_path")) {
					e.preventDefault();
					resultPanelRef?.copyQr();
				}
				// Quick screenshot (Ctrl+Shift+P): hide Lychi so it's not in the
				// shot, then capture a region. Fires from anywhere in the window.
				if (matchesAction(e, "screenshot")) {
					e.preventDefault();
					quickScreenshot();
				}
			},
			true,
		);

		// Dismiss on blur — Rust emits lychi://dismiss when GTK focus is lost
		// and dismiss is armed. Single signal, no grace periods, no race conditions.
		const unlistenDismiss = await win.listen("lychi://dismiss", () => {
			if (!hideOnBlur) return;
			if (settingsOpen || notesOpen) {
				settingsOpen = false;
				notesOpen = false;
				return;
			}
			invoke("hide_launcher").then(() => {
				if (searchMode) {
					cancelFileSearch();
					searchMode = false;
				}
				completions = [];
				completionIndex = -1;
				atMode = false;
				atStart = -1;
			});
		});
		unlisteners.push(unlistenDismiss);

		// GTK-level Escape — catches Escape even when WebView input lacks DOM focus
		const unlistenGtkEscape = await win.listen("lychi://gtk-escape", () => {
			handleDismiss();
		});
		unlisteners.push(unlistenGtkEscape);

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

async function handleSubmit(opts?: { ctrlKey?: boolean; runInline?: boolean }) {
	const trimmed = inputValue.trim();
	// Allow submit when input is empty but a context suggestion is selected
	const hasSelectedCompletion = completions.length > 0 && completionIndex >= 0;
	if ((!trimmed && !hasSelectedCompletion) || isExecuting || !backendReady) return;

	// In / search mode, Ctrl+Enter reveals the selected result in the file
	// manager (not web search — the input is a path like "/Android/", never a
	// query). Plain Enter opens; handled below via handleCompletionSelect.
	if (opts?.ctrlKey && searchMode) {
		if (hasSelectedCompletion) revealSelected();
		return;
	}

	// Ctrl+Enter: force web search regardless of completions or routing
	if (opts?.ctrlKey && trimmed) {
		await runCommand(`web ${trimmed}`);
		return;
	}

	// Shift+Enter: capture a `run` command's output inline instead of a
	// terminal (terminal is the default). Ignored by non-run handlers.
	if (opts?.runInline && trimmed) {
		await runCommand(trimmed, { runInline: true });
		return;
	}

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

	// In search mode, auto-select first selectable result (skip a leading
	// section header) if none explicitly selected.
	if (searchMode && completions.length > 0 && completionIndex < 0) {
		const first = completions.findIndex((c) => c.icon_path !== "__separator__");
		completionIndex = first >= 0 ? first : 0;
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
					auto_open: false,
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
			// The backend declared the exact command to run (search handlers,
			// emoji, etc.) — run it verbatim. No label reverse-parsing, no
			// prefix guessing. This is the single scalable path.
			// Argument-needing hint (e.g. "volume <n>"): insert the runnable
			// prefix into the input so the user types the value, then Enter
			// runs it. Tab-to-complete, a la Raycast/Alfred.
			if (selected.fill) {
				inputValue = selected.fill;
				completions = [];
				completionIndex = -1;
				handleInput(inputValue);
				return;
			}
			if (selected.run) {
				await runCommand(selected.run);
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
				"clear",
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
					// Don't re-prefix if the label already carries the prefix
					// (e.g. input "run htop", label "run htop") — that would
					// produce "run run htop". Run the label as-is in that case.
					if (selected.label.toLowerCase().startsWith(`${prefix} `)) {
						await runCommand(selected.label);
					} else {
						await runCommand(`${prefix} ${selected.label}`);
					}
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

	// In search mode with nothing selectable to act on, the user may have typed
	// or pasted a literal absolute path (e.g. "/home/sab/Android/Sdk"). Try to
	// open it directly; only if it doesn't exist do we give up (no silent no-op
	// on a valid path). The leading "/" is the search-mode trigger AND the root
	// of an absolute path, so the input value is the path as-is.
	if (searchMode) {
		const opened = await openPath(trimmed);
		if (opened) {
			inputValue = "";
			completions = [];
			completionIndex = -1;
			cancelFileSearch();
			await hide();
		}
		return;
	}

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

async function runCommand(command: string, opts?: { runInline?: boolean }) {
	if (isExecuting) return;
	isExecuting = true;
	const generation = ++executeGeneration;
	completions = [];
	completionIndex = -1;
	historyOpen = false;
	settingsOpen = false;
	mediaOpen = false;
	notesOpen = false;
	try {
		lastCommand = command;
		// Screenshot commands open a region/window selector — Lychi must leave
		// the screen first or it lands in the shot and blocks the selector. Hide
		// and let the compositor unmap before the capture starts.
		if (/^screenshot(\s|$)/i.test(command.trim())) {
			await hide();
			await new Promise((r) => setTimeout(r, 180));
		}
		const result = await executeCommand(command, undefined, opts?.runInline);
		// If this command was cancelled (Escape) while awaiting, a newer command
		// started, ignore its stale result entirely.
		if (generation !== executeGeneration) return;
		lastResult = result;
		// If confirmation is needed, show the confirm panel and wait for user decision
		if (lastResult.needs_confirmation) {
			return;
		}
		historyEntries = [...historyEntries, command];
		inputValue = "";
		// If the backend wants us to open a URI, use GDK (proper Wayland focus transfer)
		// But if output is also present (e.g. "ask" handler), show inline result instead
		if (lastResult.open_url && (lastResult.auto_open || !lastResult.output)) {
			await hide();
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
			await hide();
		}
	} catch (err) {
		if (generation !== executeGeneration) return;
		lastResult = {
			success: false,
			output: null,
			error: String(err),
			duration_ms: 0,
			auto_open: false,
		};
	} finally {
		// Only clear the running flag if we're still the current command — a
		// cancel (Escape) already reset it and may have started a new one.
		if (generation === executeGeneration) isExecuting = false;
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
		if (lastResult.open_url && (lastResult.auto_open || !lastResult.output)) {
			await hide();
			await openUri(lastResult.open_url);
		} else if (lastResult.success && !lastResult.output) {
			await hide();
		}
	} catch (err) {
		lastResult = {
			success: false,
			output: null,
			error: String(err),
			duration_ms: 0,
			auto_open: false,
		};
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
	await hide();
	await openUri(`file://${label}`);
}

/** Drill into a folder result — browse its contents inside Lychi (Tab / →). */
function drillIntoFolder(item: CompletionItem) {
	let path = item.label;
	if (path.startsWith("~/")) {
		path = path.slice(2); // ~/Downloads/ → Downloads/
	} else if (activeScope && path.startsWith(activeScope)) {
		path = path.slice(activeScope.length); // /mnt/DevSSD/Colris/ → Colris/
		if (path.startsWith("/")) path = path.slice(1);
	}
	// Prepend a single leading slash for scope-relative paths, but never double
	// it: an absolute label (that didn't match the scope-strip branch above)
	// already starts with `/`.
	inputValue = path.startsWith("/") ? path : `/${path}`;
	handleInput(inputValue);
}

/** Reveal the selected search result in the file manager (Ctrl+Enter). */
async function revealSelected() {
	const item = completions[completionIndex];
	if (!item) return;
	const full = resolveFullPath(item.label);
	if (!full) return;
	await hide();
	await revealPath(full);
}

/** Copy the selected search result's full path, flashing a confirmation. */
async function copySelectedPath() {
	const item = completions[completionIndex];
	if (!item) return;
	const full = resolveFullPath(item.label);
	if (!full) return;
	try {
		await navigator.clipboard.writeText(full);
		flashHint("Path copied");
	} catch (err) {
		console.error("[copy-path] clipboard write failed:", err);
	}
}

function handleCompletionSelect(label: string, forceOpen?: boolean) {
	// / search mode — Enter opens the folder/file; Ctrl+Enter (forceOpen) reveals
	// it in the file manager. Drilling into a folder is Tab / → (see drillIntoFolder).
	if (searchMode) {
		if (forceOpen) {
			revealSelected();
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

	// Argument-needing hint — fill the input rather than execute (matches
	// Enter-submit behaviour). Tab-to-complete.
	if (item?.fill) {
		inputValue = item.fill;
		handleInput(inputValue);
		return;
	}

	// Backend-declared command wins — run it verbatim (search handlers, emoji,
	// etc.). Keeps click-select identical to Enter-submit; no label parsing.
	if (item?.run) {
		runCommand(item.run);
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
		// If we ran off the top onto (or past) a leading header, stay put rather
		// than landing on a non-selectable separator.
		if (next < 0 || completions[next]?.icon_path === "__separator__") return;
		completionIndex = next;
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

async function hide() {
	launcherReady = false;
	await hideWindow();
}

// Quick screenshot trigger (bound to the `screenshot` keybinding). Lychi must
// leave the screen before the capture starts, or it lands in the shot — so we
// hide first, wait a beat for the compositor to actually unmap the surface,
// then fire `screenshot area`. Runs via executeCommand directly (not
// runCommand) because the window is already hidden — no panel/history churn.
async function quickScreenshot() {
	inputValue = "";
	completions = [];
	completionIndex = -1;
	await hide();
	// Give the compositor a moment to remove the window before capturing.
	await new Promise((r) => setTimeout(r, 180));
	try {
		await executeCommand("screenshot area");
	} catch (err) {
		console.error("[screenshot] quick trigger failed:", err);
	}
}

// "Browse" — open the current result's inline URL in the browser. Bound to
// the user-assigned open_inline_url keybinding and the ResultPanel button.
async function openInlineUrl() {
	if (lastResult?.open_url) {
		await hide();
		await openUri(lastResult.open_url);
	}
}

async function handleDismiss() {
	// C15: Cancel in-flight AI routing immediately on ESC
	if (isRouting) {
		routingGeneration++;
		isRouting = false;
	}

	// Escape hatch for a stuck command. If a command is mid-execution (e.g. a
	// screenshot whose portal call hung, or a backend task that died without
	// resolving its IPC promise), the first Escape *cancels* the stuck state and
	// keeps the launcher open so the user can retry — rather than hiding a window
	// that's frozen on "Running...". A second Escape then hides as usual.
	if (isExecuting) {
		executeGeneration++;
		isExecuting = false;
		lastResult = {
			success: false,
			output: null,
			error: "Cancelled",
			duration_ms: 0,
			auto_open: false,
		};
		return;
	}

	await hide();
}
</script>

<div class="launcher-wrapper" class:layer-shell={windowStrategy === 'layer-shell'} class:compact={compactMode} class:not-ready={!launcherReady} role="presentation" onmousedown={(e) => { if (!compactMode && (windowStrategy === 'x11' || windowStrategy === 'toplevel') && e.target === e.currentTarget) hide(); }} onwheel={(e) => { if (e.target === e.currentTarget) e.preventDefault(); }}>
	<div class="launcher-row" bind:this={launcherRowEl}>
	<main>
		{#if hotkeyBannerVisible}
			<div class="hotkey-banner">
				<span class="hotkey-banner-text">
					Tip: global hotkeys are limited on Wayland. For reliable summoning, bind
					<code>lychi --toggle</code> to a shortcut in your desktop's keyboard settings.
				</span>
				<button class="hotkey-banner-btn" onclick={copyToggleCommand}>
					{hotkeyBannerCopied ? "Copied" : "Copy command"}
				</button>
				<button class="hotkey-banner-btn dismiss" onclick={dismissHotkeyBanner}>
					Got it
				</button>
			</div>
		{/if}
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
			{contextLoading}
			{atMode}
			{atStart}
			{searchMode}
			{aiEnabled}
			scopeCount={mountPoints.length}
			ontabscope={() => handleScopeChange((scopeIndex + 1) % mountPoints.length)}
			ontabcomplete={() => {
				if (completions.length > 0 && completionIndex >= 0) {
					const item = completions[completionIndex];
					if (searchMode && item.icon_path === "__folder__") {
						drillIntoFolder(item);
					} else if (searchMode) {
						// File in search mode — open it
						openFileByLabel(item.label);
					} else {
						// Browse mode — existing behavior
						handleCompletionSelect(item.label);
					}
				}
			}}
			ondrillinto={() => {
				// Arrow-right → drill into the selected folder (search mode only).
				if (searchMode && completions.length > 0 && completionIndex >= 0) {
					const item = completions[completionIndex];
					if (item?.icon_path === "__folder__") drillIntoFolder(item);
				}
			}}
			oncopypath={() => {
				if (searchMode && completions.length > 0 && completionIndex >= 0) {
					copySelectedPath();
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
			<MediaPanel visible={mediaOpen} ondismiss={() => { mediaOpen = false; }} players={mediaPlayers} />
		</div>
		<div class="settings-wrapper" class:panel-hidden={!settingsOpen}>
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
				{flashMessage}
			/>
			{#if completions.length === 0 && atMode && atNoResults}
				<div class="empty-state">Empty folder</div>
			{:else if completions.length === 0 && searchMode && !searchDone}
				<div class="empty-state">Searching...</div>
			{:else if completions.length === 0 && completionsPending && !atMode && !searchMode && inputValue.trim().length > 0}
				<!-- Cold-start hint: the first query hasn't returned yet. Subtle,
				     non-blocking skeleton that vanishes the instant results arrive.
				     Input stays fully usable. -->
				<div class="skeleton-list" aria-hidden="true">
					{#each Array(4) as _, i (i)}
						<div class="skeleton-row">
							<div class="skeleton-icon"></div>
							<div class="skeleton-bar" style="width: {70 - i * 12}%"></div>
						</div>
					{/each}
				</div>
			{/if}
			{#if lastResult}
				<ResultPanel bind:this={resultPanelRef} result={lastResult} command={lastCommand}
				onconfirm={handleConfirm} ondismiss={handleConfirmDismiss}
				onopenurl={openInlineUrl}
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
		{contextStale}
		{contextStaleHint}
		{contextRefreshing}
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
		/* Fullscreen-transparent toplevel (GNOME Wayland): keep scroll events
		   from leaking through the backdrop to windows behind us */
		overscroll-behavior: none;
	}

	/* Hide until summon event clears stale state — prevents flash of previous completions */
	.launcher-wrapper.not-ready {
		opacity: 0;
		pointer-events: none;
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

	/* Non-composited X11 compact mode: the window IS the launcher (rofi-style).
	   No alpha anywhere — semi-transparent pixels render black without a
	   compositor — so: opaque background, no shadow, square corners, and the
	   window is resized to content height from a ResizeObserver. */
	.launcher-wrapper.compact {
		width: 100vw;
		height: auto;
		padding: 0;
		justify-content: flex-start;
		background: var(--bg);
	}

	.launcher-wrapper.compact .launcher-row,
	.launcher-wrapper.compact main {
		width: 100vw;
		max-width: 100vw;
	}

	.launcher-wrapper.compact main {
		max-height: none;
		border: none;
		border-radius: 0;
		box-shadow: none;
		animation: none;
	}

	/* Preview panel sits beside the bar and would be clipped by the compact
	   window — hide it (file previews need a composited session). */
	.launcher-wrapper.compact :global(.preview-panel) {
		display: none;
	}

	.launcher-row {
		position: relative;
		width: 680px;
		max-width: calc(100vw - 40px);
		align-self: flex-start;
	}

	/* First-run Wayland hotkey tip — shown once, dismissed persistently */
	.hotkey-banner {
		display: flex;
		align-items: center;
		gap: 8px;
		padding: 8px 12px;
		font-size: 11px;
		color: var(--fg-muted);
		background: var(--bg-secondary);
		border-bottom: 1px solid var(--border);
	}

	.hotkey-banner-text {
		flex: 1;
		line-height: 1.4;
	}

	.hotkey-banner-text code {
		font-family: var(--font-mono);
		background: var(--bg);
		padding: 1px 4px;
		border-radius: 3px;
		user-select: all;
	}

	.hotkey-banner-btn {
		background: transparent;
		color: var(--accent);
		border: 1px solid var(--border);
		border-radius: 4px;
		padding: 3px 8px;
		font-size: 11px;
		cursor: pointer;
		flex-shrink: 0;
		transition: background 100ms ease;
	}

	.hotkey-banner-btn:hover {
		background: var(--border);
	}

	.hotkey-banner-btn.dismiss {
		color: var(--fg-muted);
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

	/* When visible, the settings wrapper must fill main's height so the panel's
	   inner .content is the scroller and the bottom (add row) isn't clipped by
	   main's overflow:hidden. min-height:0 lets its flex child shrink to scroll. */
	.settings-wrapper {
		display: flex;
		flex-direction: column;
		flex: 1;
		min-height: 0;
	}

	.empty-state {
		padding: 12px 20px;
		font-family: var(--font-mono);
		font-size: 13px;
		color: var(--fg-muted);
		opacity: 0.6;
	}

	/* Cold-start skeleton — shimmering placeholder rows shown only while the
	   first query is in flight. Deliberately understated: it reads as "results
	   are coming", not "the app is broken/loading". */
	.skeleton-list {
		display: flex;
		flex-direction: column;
		gap: 2px;
		padding: 6px 12px;
	}

	.skeleton-row {
		display: flex;
		align-items: center;
		gap: 10px;
		padding: 7px 8px;
	}

	.skeleton-icon,
	.skeleton-bar {
		background: linear-gradient(
			90deg,
			var(--bg-secondary) 25%,
			var(--border) 50%,
			var(--bg-secondary) 75%
		);
		background-size: 200% 100%;
		animation: skeleton-shimmer 1.2s ease-in-out infinite;
		border-radius: 4px;
	}

	.skeleton-icon {
		width: 20px;
		height: 20px;
		flex-shrink: 0;
	}

	.skeleton-bar {
		height: 10px;
	}

	@keyframes skeleton-shimmer {
		0% {
			background-position: 200% 0;
		}
		100% {
			background-position: -200% 0;
		}
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

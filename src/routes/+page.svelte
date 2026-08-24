<script lang="ts">
import { LogicalSize } from "@tauri-apps/api/dpi";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { flushSync, onMount } from "svelte";
import { answerRevealPath } from "$lib/answerActions";
import ActionPanel from "$lib/components/ActionPanel.svelte";
import AiAnswer from "$lib/components/AiAnswer.svelte";
import AttachmentTray from "$lib/components/AttachmentTray.svelte";
import ChatHistoryPanel from "$lib/components/ChatHistoryPanel.svelte";
import CommandInput from "$lib/components/CommandInput.svelte";
import CompletionsList from "$lib/components/CompletionsList.svelte";
import FilePreview from "$lib/components/FilePreview.svelte";
import HistoryPanel from "$lib/components/HistoryPanel.svelte";
import MediaPanel from "$lib/components/MediaPanel.svelte";
import NotesPanel from "$lib/components/NotesPanel.svelte";
import OnboardingBanner from "$lib/components/OnboardingBanner.svelte";
import ResultPanel from "$lib/components/ResultPanel.svelte";
import SettingsPanel from "$lib/components/SettingsPanel.svelte";
import StatusBar from "$lib/components/StatusBar.svelte";
import { attachTauriEvents } from "$lib/events/bridge.svelte";
import type { CommandResult, CompletionItem } from "$lib/ipc";
import {
	addPin,
	cancelFileSearch,
	classifyInput,
	clearConversations,
	confirmExecution,
	contentText,
	deleteConversation,
	executeCommand,
	fuzzyPathCompletions,
	getActiveWindowStrategy,
	getAiStatus,
	getCompletions,
	getContext,
	getConversation,
	getConversations,
	getHideOnBlur,
	getHistory,
	getHotkeyStatus,
	getMountPoints,
	grantPrivacyConsent,
	hideWindow,
	listPathCompletions,
	loadConversation,
	openPath,
	openUri,
	readSelection,
	removePin,
	revealPath,
	runRowAction,
	saveGeneralConfig,
	saveWindowPosition,
	startFileSearch,
} from "$lib/ipc";
import { getComboString, loadKeybindings } from "$lib/keybindings";
import {
	atPartial,
	atToken,
	COPIED_TOKEN,
	expandCopiedToken,
	isEmailAt,
	parentAtPartial,
	parentSearchInput,
	parseSearchInput,
	searchScope,
	spliceAtToken,
} from "$lib/modes";
import { type BannerMode, bannerMode } from "$lib/onboarding";
import { hasVisibleOutput, resolveOutput } from "$lib/output";
import { preloadAll } from "$lib/preloadCache";
import { MARKER, PANEL, PREFIX } from "$lib/sentinels";
import { attachments } from "$lib/stores/attachments.svelte";
import { type AiTurn, chat, shouldResume, type UserAttachment } from "$lib/stores/chat.svelte";
import { completions } from "$lib/stores/completions.svelte";
import { context } from "$lib/stores/context.svelte";
import { media } from "$lib/stores/media.svelte";
import { ui } from "$lib/stores/ui.svelte";
import {
	decideSubmit,
	type RouteDecision,
	renderPreset,
	type SubmitAction,
} from "$lib/submit-router";
import { installUiLogging, uiLog } from "$lib/uiLog";

let inputValue = $state("");
let isExecuting = $state(false);
// Bumped when a stuck/in-flight command is cancelled via Escape, so a late
// resolution of the abandoned executeCommand promise is ignored instead of
// clobbering fresh state.
let executeGeneration = 0;
// Local-AI model warmup indicator now lives in the `ui` store (`ui.aiLoading`);
// only fires for mode=local (BYOK/Ollama/Cloud have no local model to load).
let backendReady = $state(false);
let lastResult: CommandResult | null = $state(null);
let lastCommand = $state("");

// --- AI agent (streaming, tool-calling) state ---
// All conversation state now lives in the `chat` store singleton
// ($lib/stores/chat.svelte) — the streamed answer, tool-step lifecycle, pending
// approval, transcript, token/truncation bookkeeping, and the `gen` staleness
// guard. This page keeps only thin orchestration wrappers (the parts that also
// touch page state like `lastResult` or call `runCommand`).
// AI-answer visibility lives in the `ui` surface machine: `ui.aiVisible`
// (on stage), `ui.aiParked` (exists but off-stage), `ui.aiExists` (either).
// Ref to the AiAnswer component, so explicit actions (Full chat / recall) can
// shift focus to its reply box. The launcher input stays primary otherwise.
let aiAnswerRef: AiAnswer | undefined = $state();

let historyEntries: string[] = $state([]);
// Panel visibility (history / chat-history / notes / media / settings) now lives
// in the `ui` surface machine — read via `ui.panelVisible("…")`, mutate via
// `ui.togglePanel/openPanel/closePanel`. Chat-history recall = the `chat` keyword.
let conversations: import("$lib/ipc").ConversationSummary[] = $state([]);

// The completion list, `@`-browse, and `/`-file-search state now live in the
// `completions` store singleton ($lib/stores/completions.svelte). Read/mutate via
// completions.items / .index / .pending / .atMode / .searchMode / etc. The two
// breadcrumb `$derived.by` below stay here — they read the page's `inputValue`.

// Derive breadcrumb path context for / search mode (when drilled into a folder)
let searchPathContext = $derived.by(() => {
	if (!completions.searchMode || !completions.searchScopePath) return "";
	const home = completions.mountPoints[0]?.path ?? "";
	if (home && completions.searchScopePath.startsWith(home)) {
		return `~${completions.searchScopePath.slice(home.length)}/`;
	}
	return `${completions.searchScopePath}/`;
});

// Derive breadcrumb path context for @ mode. `atPartial` gives the text after
// the @ (token-bounded, same boundary the splices use); the rest is
// breadcrumb-specific display formatting (the ~/ prefix).
let atPathContext = $derived.by(() => {
	if (!completions.atMode || completions.atStart < 0) return "";
	const partial = atPartial(inputValue, completions.atStart);
	const lastSlash = partial.lastIndexOf("/");
	if (lastSlash === -1) return "~/";
	const raw = partial.slice(0, lastSlash + 1);
	return raw.startsWith("/") ? raw : `~/${raw}`;
});

let hideOnBlur = $state(true);
let windowStrategy = $state("x11"); // "layer-shell" or "x11" — drives layout mode
// Non-composited X11 (xfwm4/Marco with compositing off): the window is a
// compact opaque rofi-style box sized to content — transparency would render
// black. Drives the .compact/.no-compositor layout and dynamic window resize.
let compactMode = $state(false);
let launcherRowEl: HTMLDivElement | undefined = $state();

// Keep the compact window's height matched to content (rofi-style resize).
// Debounced: coalesces bursts of content changes (e.g. suggestions landing just
// after the window shows) into a SINGLE resize, so the window doesn't visibly
// jump twice on open. Also ignores sub-pixel/tiny deltas that aren't real
// content changes.
$effect(() => {
	if (!compactMode || !launcherRowEl || !("__TAURI_INTERNALS__" in window)) return;
	const el = launcherRowEl;
	const win = getCurrentWindow();
	let lastH = 0;
	let raf = 0;
	const apply = () => {
		raf = 0;
		const maxH = Math.round(window.screen.height * 0.6);
		const h = Math.min(Math.ceil(el.getBoundingClientRect().height), maxH);
		// Ignore ≤1px jitter — only resize on a real content-height change.
		if (h > 0 && Math.abs(h - lastH) > 1) {
			lastH = h;
			win.setSize(new LogicalSize(680, h)).catch(() => {});
		}
	};
	const observer = new ResizeObserver(() => {
		// Coalesce to the next frame — several observer callbacks in one tick
		// (suggestions arriving) collapse into one setSize.
		if (raf) cancelAnimationFrame(raf);
		raf = requestAnimationFrame(apply);
	});
	observer.observe(el);
	return () => {
		if (raf) cancelAnimationFrame(raf);
		observer.disconnect();
	};
});
// First-run hotkey onboarding: shown when the hotkey is broken, or when it is
// merely *unproven* and pressing it is the only way to find out.
//
// This used to be gated on `session_type === "wayland"`, so the X11 case where
// the window manager silently swallows the key — the one that actually
// stranded a tester on XFCE — marked onboarding complete and said nothing.
let hotkeyBannerVisible = $state(false);
let hotkeyBannerCopied = $state(false);
/// What the first-run banner is for:
/// - "confirm": the hotkey grab is unproven — ask the user to press it.
/// - "broken":  it cannot work here — offer a rebind and `lychi --toggle`.
/// - "welcome": the hotkey is fine, so point at the Guide instead.
///
/// The third case is the H3 gap: a user whose hotkey worked previously had
/// onboarding marked complete having been shown nothing at all.
let hotkeyBannerMode = $state<BannerMode>("welcome");
let hotkeyBannerText = $state("");
let settingsPanel = $state<SettingsPanel | undefined>();
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

/// Open Settings at a specific tab and treat that as having seen onboarding.
///
/// Dismisses first so the flag is persisted even if the panel fails to open —
/// a banner that reappears every launch is worse than one shown once too few.
function openSettingsFromBanner(tab: "general" | "guide" | "setup") {
	dismissHotkeyBanner();
	settingsPanel?.showTab(tab);
	if (!ui.panelVisible("settings")) handleToggleSettings();
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

// Environment context (active window / cwd / git) + its freshness now live in the
// `context` store: `context.env`, `context.stale`/`context.staleHint`,
// `context.refreshing`, `context.loading`, `context.pill`, `context.extractStale()`.

/**
 * Load the empty-input suggestions (recents / context rows) into the completions
 * list. Called immediately on summon (so recents appear at once, not after the
 * context round-trip) AND again when fresh context arrives (to enrich in place).
 * Guarded by `completionGen` so a typist's query is never overwritten, and only
 * applies while the input is still empty.
 */
/** Sentinel prefix stashed in a completion's `run` to mark it as an AI-chat
 *  recall row: `__chat__:<conversationId>`. Chosen → opens the conversation. */
const CHAT_RUN_PREFIX = PREFIX.chat;

function loadEmptySuggestions() {
	if (inputValue.trim().length > 0) return;
	const gen = ++completions.completionGen;
	// Recents only. AI chats are NOT mixed in here: no comparable launcher
	// interleaves conversations with commands (Raycast keeps chat history inside
	// the AI surface; Claude's quick entry gates it behind an explicit click),
	// and four chat rows pushed the actual commands below the fold. Conversations
	// are recalled via the `chat` keyword and the History panel — one place each.
	getCompletions("")
		.then((rawResults) => {
			if (gen !== completions.completionGen || inputValue.trim().length > 0) return;
			const next = context.extractStale(rawResults);
			// This runs twice per summon (immediately, then on context-ready).
			// When the second fetch matches what's showing, don't reassign:
			// a wholesale replace resets selection and visibly reshuffles a
			// list the user may already be arrowing through.
			const rowKey = (i: CompletionItem) =>
				`${i.label} ${i.run ?? ""} ${i.icon_path ?? ""} ${i.description ?? ""} ${i.pinned ?? false} ${i.reason ?? ""}`;
			if (
				next.length === completions.items.length &&
				next.every((item, i) => rowKey(item) === rowKey(completions.items[i]))
			) {
				return;
			}
			// Re-locate a row the user had selected; fall back to no selection.
			const selected = completions.index >= 0 ? completions.items[completions.index] : undefined;
			completions.items = next;
			// Do NOT preselect: on an empty box these are recents, not a query
			// result. Selection happens only on mouse press or arrow key.
			completions.index = selected
				? next.findIndex((item) => rowKey(item) === rowKey(selected))
				: -1;
		})
		// Discarding this left a permanently empty launcher with nothing to
		// distinguish "no recents yet" from "the backend call failed". Logged
		// rather than flashed: this runs on every summon, and a toast on an
		// empty box would be noise the user cannot act on.
		.catch((err) => {
			uiLog.error(`[suggestions] empty-state load failed: ${err}`);
		});
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

let resultPanelRef: ResultPanel | undefined = $state(undefined);
let pendingNoteText: string | null = $state(null);
let initialNotesTab: "notes" | "todos" | "reminders" | "timers" | "snippets" | undefined =
	$state(undefined);
// MPRIS players + now-playing live in the `media` store (`media.players`,
// `media.nowPlaying`, `media.multiplePlayers`); its poll loop is started in onMount.
// Whether an AI provider is actually configured — gates the natural-language
// placeholder suggestions so we never advertise AI-only actions to a user who
// has no key (they'd silently web-search instead).
let aiEnabled = $state(false);

// File preview — show for files and folders in search or browse mode
let previewPath = $derived.by(() => {
	if (!completions.searchMode && !completions.atMode) return "";
	if (completions.items.length === 0 || completions.index < 0) return "";
	const item = completions.items[completions.index];
	if (!item) return "";
	return resolveFullPath(item.label);
});
let showPreview = $derived(previewPath.length > 0);

// A partial is "path-like" (spelling out a path → drill/browse) vs a bare name
// (fuzzy jump-to-file anywhere, Claude-Code style). Path-like = contains a
// slash or starts with ~ / an absolute /.
function isPathLikeAt(partial: string): boolean {
	return partial.includes("/") || partial.startsWith("~");
}

// @ mode completion fetch. Bare queries fuzzy-jump across the whole home index;
// path-like queries (drilling into a dir, or a typed path) list that directory.
/**
 * Ambient context sources offered alongside file results in `@` mode. These
 * aren't paths — the backend resolves them at send time (`expand_context_refs`)
 * — but they share the `@` syntax, so they belong in the same picker. Listed
 * first when they match, since they're short and exact.
 */
const AT_CONTEXT_SOURCES = [
	{ keyword: "clipboard", description: "What you last copied" },
	{ keyword: "selection", description: "Text highlighted in the focused window" },
];

function contextCompletions(partial: string): CompletionItem[] {
	const q = partial.toLowerCase();
	return AT_CONTEXT_SOURCES.filter((s) => s.keyword.startsWith(q)).map(
		(s) =>
			({
				label: `@${s.keyword}`,
				description: s.description,
				// `fill` completes the token in place, matching how other
				// argument-hint rows behave.
				fill: `@${s.keyword} `,
				icon_path: "__context__",
			}) as CompletionItem,
	);
}

async function fetchAtCompletions(partial: string) {
	try {
		const results =
			partial.length > 0 && !isPathLikeAt(partial)
				? await fuzzyPathCompletions(partial)
				: await listPathCompletions(partial);
		// Context sources lead — they're exact short matches, and a path query
		// (`~/`, `/`, `.`) filters them out naturally since none start that way.
		const merged = [...contextCompletions(partial), ...results];
		// Defer state update to next frame so it never blocks a keystroke paint
		requestAnimationFrame(() => {
			completions.items = merged;
			completions.index = merged.length > 0 ? 0 : -1;
			completions.atNoResults = merged.length === 0 && partial.length > 0;
		});
		// Stay in @ mode even when empty — user can backspace to navigate up
	} catch (err) {
		console.error("[@completions] error:", err);
		// Stay in @ mode on error too — don't kick the user out
		requestAnimationFrame(() => {
			completions.items = [];
			completions.index = -1;
			completions.atNoResults = true;
		});
	}
}

// Resolve the absolute path for a completion label (for preview)
function resolveFullPath(label: string): string {
	const mapped = completions.filePathMap.get(label);
	if (mapped) return mapped;
	// Browse mode: labels like "~/foo/bar.txt"
	if (label.startsWith("~/")) {
		const home = completions.mountPoints[0]?.path ?? "";
		return `${home}/${label.slice(2)}`;
	}
	return label.endsWith("/") ? label.slice(0, -1) : label;
}

// A large text paste is staged HERE, off the input — the input shows only the
// atomic [copied text] token (see CommandInput.handlePaste) and the payload
// expands back into the query at submit. One decider for the payload's life:
// the effect below drops it the moment the token leaves the input, whatever
// removed it (atomic delete, submit clearing the box, Escape, a fill).
let pastedPayload: string | null = $state(null);

function stagePastedText(text: string): boolean {
	pastedPayload = text;
	return true;
}

$effect(() => {
	if (pastedPayload !== null && !inputValue.includes(COPIED_TOKEN)) {
		pastedPayload = null;
	}
});

// Called by CommandInput on every keystroke
function handleInput(val: string) {
	// Skip completions until backend is ready — input still updates visually via bind:value
	if (!backendReady) return;
	// Close any open panel so CompletionsList renders, and force the results surface.
	ui.showLauncher();
	// Typing a new query dismisses an on-screen AI answer so completions show.
	// (The conversation is already persisted; recall it via `chat` if needed.)
	if (ui.aiExists && val.trim().length > 0) {
		chat.reset();
	}
	// Same rule for a result card: typing a new query means the previous output is
	// no longer what you're looking at, so it yields the surface back to the
	// suggestions. Without this the output gate (see `completions.outputShown`)
	// would keep the list hidden while you typed the next command.
	if (lastResult && val.trim().length > 0) {
		lastResult = null;
	}
	clearTimeout(completions.debounceTimer);
	completions.atNoResults = false;

	// Detect / file search mode — must be at the start of input
	if (val.startsWith("/")) {
		const raw = val.slice(1);

		// Parse path first: /folder/subfolder/query → folder=folder/subfolder, term=query
		const parsed = parseSearchInput(raw);

		// Only reject if the TERM (part after last /) has a space — folder paths can have spaces
		if (!parsed.termHasSpace) {
			// Exit @ mode if active
			completions.atMode = false;
			completions.atStart = -1;

			// Refresh mount points on entering search mode (detects new USB drives etc.)
			if (!completions.searchMode) {
				getMountPoints().then((mounts) => {
					completions.mountPoints = mounts;
				});
			}

			completions.searchMode = true;
			completions.searchDone = false;
			completions.fileSearchId++;
			const id = completions.fileSearchId;

			const baseScope = completions.activeScope || (completions.mountPoints[0]?.path ?? "");
			const scope = searchScope(baseScope, parsed);
			const searchTerm = parsed.term;
			// Only a folder-qualified search publishes its scope path (drives the
			// breadcrumb); a top-level query leaves it empty as before.
			completions.searchScopePath = parsed.hasFolder ? scope : "";

			// Keep the current ROWS on screen until the replacement batch lands, but
			// still drop the SELECTION.
			//
			// Clearing `items` here is what made the list flicker: every keystroke
			// emptied it, and the empty-state branch renders "Searching..." whenever
			// `items` is empty and the search is unfinished — so each keypress
			// flashed results → "Searching..." → results. Each batch carries the
			// FULL ranked set for its query and replaces `items` wholesale, so there
			// is nothing to clear: stale rows are overwritten, not appended to.
			//
			// `index` is a different matter and MUST be reset. Submit resolves the
			// selected row via `items[index]`, so an index left pointing into the
			// previous query's list makes Enter run a leftover row instead of what
			// the user typed — which looked like "the AI never responds".
			if (raw.length > 0 || parsed.hasFolder) {
				completions.index = -1;
				completions.debounceTimer = setTimeout(() => {
					if (scope) startFileSearch(searchTerm, scope, id);
				}, 150);
			} else {
				// Just "/" typed — list the active scope immediately (home dir by default)
				completions.items = [];
				completions.index = -1;
				if (scope) startFileSearch("", scope, id);
			}
			return;
		}
	}

	// Exiting search mode
	if (completions.searchMode) {
		cancelFileSearch();
		completions.exitSearch();
	}

	// Detect @ file reference — find the last @ in the input
	const atIdx = val.lastIndexOf("@");
	if (atIdx !== -1) {
		const partial = val.slice(atIdx + 1);
		// Skip if it looks like an email (non-space chars before @) — see modes.ts
		if (!partial.includes(" ") && !isEmailAt(val, atIdx)) {
			completions.atMode = true;
			completions.atStart = atIdx;
			cancelFileSearch();
			const gen = ++completions.completionGen;
			const fetchAt =
				partial.length > 0 && !isPathLikeAt(partial)
					? fuzzyPathCompletions(partial)
					: listPathCompletions(partial);
			fetchAt
				.then((results) => {
					if (gen !== completions.completionGen) return;
					requestAnimationFrame(() => {
						completions.items = results;
						completions.index = results.length > 0 ? 0 : -1;
						completions.atNoResults = results.length === 0 && partial.length > 0;
					});
				})
				.catch((err) => {
					console.error("[@completions] error:", err);
					if (gen !== completions.completionGen) return;
					requestAnimationFrame(() => {
						completions.items = [];
						completions.index = -1;
						completions.atNoResults = true;
					});
				});
			return;
		}
	}

	// Not in @ or / mode — normal completions
	completions.atMode = false;
	completions.atStart = -1;

	const trimmed = val.trim();
	if (trimmed.length < 1 && !context.env) {
		completions.completionGen++;
		completions.pending = false;
		completions.items = [];
		completions.index = -1;
		return;
	}

	const gen = ++completions.completionGen;
	completions.pending = true;
	getCompletions(trimmed)
		.then((rawResults) => {
			if (gen !== completions.completionGen) return;
			completions.pending = false;
			const results = context.extractStale(rawResults);
			completions.items = results;
			// Raycast/Alfred single-list model: the top real result is
			// auto-selected, so plain Enter runs it.
			//
			// WHICH row may be auto-selected is the backend's verdict, not a
			// judgement made here. `can_be_default` is stamped by the ranker,
			// which is the only place that knows the row's Source (a fallback or
			// a guard may never be Enter's target) and its Tier (only an
			// identity or prefix match may). This used to be re-derived from
			// display text with a `startsWith` check, which reimplemented one of
			// those three conditions and dropped the other two.
			const defaultable = results.findIndex((c) => c.can_be_default === true);
			// When nothing real is defaultable, select the leading fallback (the
			// backend orders "Ask AI" first when AI is on, else "Search web").
			// These are safe to run on Enter — they only take the typed query —
			// and a query that matched no command should still act on Enter, not
			// leave the list with nothing selected.
			const leadFallback = results.findIndex((c) => c.kind === "ask-ai" || c.kind === "search-web");
			completions.index = defaultable >= 0 ? defaultable : leadFallback;
		})
		.catch((err) => {
			if (gen === completions.completionGen) completions.pending = false;
			console.error("[completions] error:", err);
		});
}

onMount(() => {
	// First thing in onMount: an uncaught error thrown before this runs is
	// invisible to the log file, and a frontend crash otherwise leaves a log
	// that looks perfectly healthy.
	installUiLogging();
	// Each of these degrades one feature on failure and none should take down
	// the launcher, but an unguarded `.then()` also leaves NO trace: the symptom
	// is a launcher quietly missing a capability with a clean-looking log.
	// `startup` keeps them independent and makes each failure a logged fact.
	// A function declaration, not an arrow: `<T>` in an arrow is ambiguous
	// enough that the bumped Biome's lint-fix and formatter disagree about a
	// trailing comma and undo each other. The declaration form parses
	// unambiguously, so both leave it alone.
	function startup<T>(what: string, p: Promise<T>, apply: (v: T) => void) {
		p.then(apply).catch((err) => {
			uiLog.error(`[startup] ${what} failed: ${err}`);
		});
	}

	startup("window strategy", getActiveWindowStrategy(), (s) => {
		windowStrategy = s;
	});
	startup("hide-on-blur", getHideOnBlur(), (v) => {
		hideOnBlur = v;
	});
	startup("AI status", getAiStatus(), (s) => {
		aiEnabled = s.has_ai_router;
	});
	startup("mount points", getMountPoints(), (mounts) => {
		completions.mountPoints = mounts;
	});
	startup("history", getHistory(), (entries) => {
		historyEntries = entries;
	});
	Promise.all([preloadAll(), getCompletions(MARKER.warmup).catch(() => {})])
		.then(([[settings]]) => {
			loadKeybindings(settings.keybindingsConfig);
			loadedGeneralConfig = settings.generalConfig;
			compactMode = settings.activeWindowStrategy === "x11" && !settings.screenComposited;
			// First-run onboarding: on Wayland the in-app hotkey only fires over
			// XWayland windows — point the user at a DE-bound `lychi --toggle`.
			if (!settings.generalConfig.first_run_completed) {
				// Every first run says something now: which thing is decided by
				// `bannerMode`, not restated here.
				getHotkeyStatus()
					.then((status) => {
						hotkeyBannerMode = bannerMode(status);
						hotkeyBannerText = status.explanation;
						hotkeyBannerVisible = true;
					})
					.catch(() => {
						hotkeyBannerMode = bannerMode(null);
						hotkeyBannerVisible = true;
					});
			}
		})
		// A throw inside the handler above (not just a rejected preload) would
		// otherwise be an unhandled rejection with no log line.
		.catch((err) => {
			uiLog.error(`[startup] preload failed: ${err}`);
		})
		.finally(() => {
			backendReady = true;
			if (inputValue.trim()) handleInput(inputValue);
		});

	// `.finally` above covers rejection but NOT a hang: a preload that never
	// settles leaves `backendReady` false forever, and submit is gated on it —
	// so the launcher accepts keystrokes and silently ignores Enter, which is
	// exactly the "intermittent no-response" symptom. Time it out instead.
	setTimeout(() => {
		if (!backendReady) {
			uiLog.error("[startup] preload did not settle in 10s — unblocking input");
			backendReady = true;
			if (inputValue.trim()) handleInput(inputValue);
		}
	}, 10_000);

	// Guard: only attach Tauri listeners if running inside Tauri
	if (!("__TAURI_INTERNALS__" in window)) return;

	// Start the MPRIS status poll (5s active / 30s idle); dispose on teardown.
	const stopMediaPoll = media.start();

	// All lychi:// event wiring lives in the bridge (transport); store-only events
	// route through the pure router, the page-coupled ones come back through these
	// handlers. onMount stays a thin composition root.
	let detachEvents: (() => void) | undefined;
	attachTauriEvents({
		ready: () => {
			launcherReady = true;
		},
		summon: handleSummon,
		afterContextReady: () => {
			// Fresh context arrived — re-fetch suggestions to enrich them in place
			// (guarded, empty-input-only). The fixed-pool CompletionsList updates
			// without DOM churn, so this never causes a jump.
			loadEmptySuggestions();
		},
		dismiss: handleBlurDismiss,
		requestHide: () => {
			// Hotkey toggle asked us to hide: blank + paint, then hide (flash fix).
			void blankThenHide();
		},
		escape: handleDismiss,
		aiStatusChanged: (available) => {
			aiEnabled = available;
		},
		windowMoved: (x, y) => saveWindowPosition(x, y),
		keyEffects: {
			hasInlineUrl: () => !!lastResult?.open_url,
			// Asks the decider whether this result can be copied as an image,
			// rather than re-deriving it from `output_type` here. The previous
			// `output_type === "svg"` was a second site matching the same field
			// ResultPanel matched, so adding an image-ish output type would have
			// needed both updated — and only one of them would have been.
			isSvgResult: () =>
				!!lastResult && resolveOutput(lastResult).actions.some((a) => a.id === "copy_image"),
			openInlineUrl,
			copyQr: () => resultPanelRef?.copyQr(),
			screenshot: quickScreenshot,
		},
	}).then((off) => {
		detachEvents = off;
	});

	return () => {
		stopMediaPoll();
		detachEvents?.();
	};
});

/** Full summon reset: pristine launcher, fresh chat/context, focus the input. */
function handleSummon() {
	// A per-summon heartbeat, so frontend SILENCE becomes meaningful.
	//
	// Without it the log cannot separate "the UI is fine and had nothing to
	// report" from "the UI died after startup" — both look like an absence of
	// `[ui]` lines. That ambiguity is the whole reason a tester's blank window
	// was misread as a crash: the backend kept logging happily while the
	// frontend was the part in trouble. One line per open means a summon with
	// no matching `[ui] summon` is now a fact, not a guess.
	uiLog.info("[ui] summon — launcher surface ready");

	// Resume path: a run was still ACTIVE (streaming or awaiting approval) when the
	// user clicked away. The backend agent never stopped (hiding doesn't cancel
	// it), so instead of wiping the conversation we PRESERVE it and show a "resumed"
	// banner. We keep the AI surface mounted (no ui.summonReset / chat.reset, so
	// `chat.gen` is unchanged and the still-arriving agent events keep landing) and
	// skip loadEmptySuggestions (the AI surface owns the view). This also removes
	// the reset-after-paint "flash" for this case by construction. Esc/Stop, or the
	// banner's Start-fresh, are the ways out.
	if (shouldResume(chat)) {
		uiLog.info("[ui] summon — resuming an active agent run (preserved)");
		chat.resumed = true;
		inputValue = "";
		// The preserved AI surface is already in a clean state — reveal now.
		launcherReady = true;
		requestAnimationFrame(() => {
			document.querySelector<HTMLInputElement>(".input-container input")?.focus();
		});
		setTimeout(() => {
			document.querySelector<HTMLInputElement>(".input-container input")?.focus();
		}, 50);
		return;
	}

	inputValue = "";
	lastResult = null;
	// Pristine launcher surface: no panel, nothing parked.
	ui.summonReset();
	// Fresh AI conversation each summon (decision: reset-every-summon, unless a run
	// was active — handled by the resume path above).
	chat.reset();
	// NOTE: preset keyword matching now lives in the backend classifier, so the
	// FE no longer caches presets for routing. `aiEnabled` is NOT re-fetched here
	// either — it's kept live by the
	// `lychi://ai-status-changed` event (see onMount), which the AiReactor emits
	// whenever the provider is (re)built. Event-driven, not polled.
	// Clear stale context immediately — fast path re-populates if same window,
	// fresh gather populates for changed windows. Prevents flash of wrong context.
	// (context.reset also arms the >120ms delayed-skeleton timer.)
	context.reset();

	completions.index = -1;
	completions.atMode = false;
	completions.atStart = -1;
	completions.searchMode = false;
	cancelFileSearch();
	// Load recents/suggestions IMMEDIATELY (don't wait for context-ready) so they're
	// present as the window appears — no post-paint pop-in. Fresh context re-fetches
	// to enrich in place. We keep the old completions until the new ones arrive.
	loadEmptySuggestions();
	// Reveal the surface only AFTER the reset is fully applied. `launcherReady`
	// gates both the wrapper's visibility AND whether the panels are even in the
	// DOM (see the template), so panels mount for the first time in an
	// already-clean state — they can never paint a stale/open panel from the
	// previous session on the way in. Then flushSync commits panels-closed +
	// surface-reset + completions-cleared + the reveal SYNCHRONOUSLY, before the
	// backend's post-summon `window.show()` maps the window (Svelte otherwise
	// flushes on a microtask, leaving a frame where the old state could show).
	launcherReady = true;
	flushSync();
	// Force focus the input (layer shell may not auto-focus DOM elements). Double-tap:
	// rAF for immediate attempt, setTimeout for a delayed retry in case the
	// compositor grants surface focus slightly late.
	requestAnimationFrame(() => {
		document.querySelector<HTMLInputElement>(".input-container input")?.focus();
	});
	setTimeout(() => {
		document.querySelector<HTMLInputElement>(".input-container input")?.focus();
	}, 50);
}

/** Discard a resumed run from its banner: do the reset the summon deferred. */
function startFreshFromResume() {
	chat.cancel(); // stop the still-running backend agent — the user chose fresh
	chat.reset();
	ui.summonReset();
	lastResult = null;
	loadEmptySuggestions();
	requestAnimationFrame(() => {
		document.querySelector<HTMLInputElement>(".input-container input")?.focus();
	});
}

/** Blur-dismiss (lychi://dismiss): respect hide-on-blur; an open Settings/Notes
 *  panel just closes instead of hiding the window. */
function handleBlurDismiss() {
	// Logged because the backend's DISMISS decision is not the same thing as the
	// window closing: this function can decline (hide-on-blur off) or redirect
	// (close a panel instead). Previously none of those outcomes reached the log,
	// so "[dismiss] → DISMISS" was the last thing anyone could see and the three
	// possible endings were indistinguishable.
	uiLog.info(
		`[dismiss] frontend received dismiss (hideOnBlur=${hideOnBlur}, ` +
			`settings=${ui.panelVisible("settings")}, notes=${ui.panelVisible("notes")})`,
	);
	if (!hideOnBlur) {
		uiLog.info("[dismiss] ignored — hide-on-blur is off");
		return;
	}
	if (ui.panelVisible("settings") || ui.panelVisible("notes")) {
		uiLog.info("[dismiss] closing open panel instead of hiding window");
		ui.closePanel();
		return;
	}
	uiLog.info("[dismiss] hiding window");
	blankThenHide().then(() => {
		if (completions.searchMode) {
			cancelFileSearch();
			completions.searchMode = false;
		}
		completions.items = [];
		completions.index = -1;
		completions.atMode = false;
		completions.atStart = -1;
	});
}

/**
 * Recall a stored conversation (from the `chat` panel): prime the backend
 * session with its history, render its turns on the AI surface, and show the
 * reply box so the next message continues it. No new AI request is made — the
 * user's follow-up drives the next turn.
 */
async function openConversation(id: string) {
	// A user clicked this row; returning silently looks like a dead click.
	const conv = await loadConversation(id).catch((err) => {
		uiLog.error(`[chat] loadConversation(${id}) failed: ${err}`);
		flashHint("Couldn't open that conversation");
		return null;
	});
	if (!conv) return;
	// ui.showAi() below closes the chat-history panel as it takes the surface.
	// Pair the stored messages into user→assistant turns for the transcript.
	// Tool/system messages are skipped in this view (the raw session still has
	// them for the model's context).
	const turns: AiTurn[] = [];
	let pendingUser = { instruction: "", attachment: undefined as UserAttachment | undefined };
	for (const m of conv.messages) {
		// `content` is a block list since vision landed (text interleaved with
		// images) — flatten to the prose for display. Mirrors the Rust
		// `ChatMessage::content_text()`; images are shown via the turn's own
		// file chips, not re-rendered from history.
		const text = contentText(m.content);
		if (m.role === "user") {
			// A folded turn carries its own display split (computed by the sender
			// it when the message was SENT and it was persisted with the message),
			// so recall replays that verdict rather than re-deriving the boundary
			// from flat text — one decider, no drift. Unfolded turns render as-is.
			pendingUser = m.display
				? {
						instruction: m.display.instruction,
						attachment: { label: m.display.label, body: m.display.body },
					}
				: { instruction: text, attachment: undefined };
		} else if (m.role === "assistant" && text) {
			turns.push({
				user: pendingUser.instruction,
				text,
				toolSteps: [],
				attachment: pendingUser.attachment,
			});
			pendingUser = { instruction: "", attachment: undefined };
		}
	}
	const prior = turns.slice(0, -1); // last pair goes into the live slot
	const last = turns[turns.length - 1];
	// loadRecalled sets the transcript + live slot, shows the AI surface, and bumps
	// the generation for any follow-up.
	chat.loadRecalled(
		prior,
		last?.user ?? pendingUser.instruction,
		last?.text ?? "",
		last ? last.attachment : pendingUser.attachment,
	);
	lastResult = null;
	// Opening a recalled conversation is an explicit action → focus the reply box.
	aiAnswerRef?.focusReply();
}

/** Delete one stored conversation from the recall list. */
async function deleteConversationEntry(id: string) {
	// Only drop the row if the delete actually happened. Filtering regardless
	// showed the conversation vanishing and then reappearing on next summon.
	try {
		await deleteConversation(id);
	} catch (err) {
		uiLog.error(`[chat] deleteConversation(${id}) failed: ${err}`);
		flashHint("Couldn't delete that conversation");
		return;
	}
	conversations = conversations.filter((c) => c.id !== id);
}

/** Clear all stored conversations. */
async function clearAllConversations() {
	try {
		await clearConversations();
	} catch (err) {
		uiLog.error(`[chat] clearConversations failed: ${err}`);
		flashHint("Couldn't clear conversations");
		return;
	}
	conversations = [];
}

async function handleSubmit(opts?: { ctrlKey?: boolean; runInline?: boolean }) {
	if (isExecuting || !backendReady) return;
	beginSubmit();

	// A settled AI answer that produced a path + an empty input → Enter opens the
	// folder (the "Open folder" action's primary binding). Same detector the chip
	// uses, so key and button always agree. A modifier or typed text falls through
	// to normal routing. When focus is in the follow-up reply box instead, its own
	// keydown handles this and stops propagation before we get here.
	if (ui.aiVisible && !chat.streaming && !inputValue.trim() && !opts?.ctrlKey) {
		const path = answerRevealPath(chat.text);
		if (path) {
			await revealAnswerPath(path);
			return;
		}
	}

	// String CLASSIFICATION is the backend's job (the single source of truth):
	// `classifyInput` returns a typed RouteDecision the FE reducer actuates. The
	// FE never re-derives command-vs-AI from its own keyword list. We fetch the
	// decision for the raw input, plus the selected row's `run` when it differs
	// (so a replayed history/context command routes exactly as if typed fresh).
	// KEYBOARD/MODE outcomes (Ctrl/Shift, /-search, @-browse) need no decision, so
	// skip the round-trip when a modifier or search/@ mode already determines it.
	// SNAPSHOT everything this Enter acts on BEFORE any await. The classify
	// round-trip is ~2-3ms, and a completions batch landing in that window used
	// to swap the array under decideSubmit — which re-derives the selected row
	// from items[index], so the stale runDecision applied to whatever row now
	// sat at that position: the "ran the wrong thing" class, unreproducible by
	// design. This Enter belongs to what was on screen when it was pressed.
	const rawTrimmed = inputValue.trim();
	// The [copied text] token expands to its staged payload HERE — classification
	// and execution see the real text, while the input keeps showing the token.
	const trimmed = expandCopiedToken(rawTrimmed, pastedPayload);
	const searchMode = completions.searchMode;
	const atMode = completions.atMode;
	const items = completions.items;
	const index = completions.index;
	const needsDecision = !opts?.ctrlKey && !opts?.runInline && !searchMode && !atMode;
	const selected = index >= 0 ? items[index] : undefined;
	const selectedRun = selected?.run ?? undefined;

	let inputDecision: RouteDecision | undefined;
	let runDecision: RouteDecision | undefined;
	if (needsDecision) {
		// ~2-3ms warm; gated by backendReady above.
		//
		// A classifier failure used to be swallowed to `undefined`, and the
		// reducer treats "no decision" as a race it expects the caller to retry
		// — but the caller had already awaited, so Enter silently did NOTHING.
		// That is invisible to the user and indistinguishable from a hang, in a
		// bug class ("intermittent no-response") that has cost real time. A
		// failure now says so and is logged; the input is left intact to retry.
		let classifyFailed: unknown;
		const classify = (q: string) =>
			classifyInput(q).catch((err) => {
				classifyFailed ??= err;
				return undefined;
			});
		[inputDecision, runDecision] = await Promise.all([
			trimmed ? classify(trimmed) : Promise.resolve(undefined),
			selectedRun && selectedRun !== trimmed ? classify(selectedRun) : Promise.resolve(undefined),
		]);
		if (classifyFailed !== undefined) {
			uiLog.error(`[submit] classifyInput failed: ${classifyFailed}`);
			flashHint("Couldn't route that — press Enter to retry");
			return;
		}
	}

	// The user kept typing during the await: the decision belongs to text that
	// is no longer in the box. Executing it would run something the user can't
	// see anymore — drop this Enter; the next one classifies the current text.
	if (inputValue.trim() !== rawTrimmed) {
		return;
	}

	// The FE reducer folds the backend decision with keyboard/mode/selection
	// state (src/lib/submit-router.ts) into exactly one SubmitAction — fed the
	// SNAPSHOTS from above, never the live completions state.
	const action = decideSubmit({
		trimmed,
		ctrlKey: opts?.ctrlKey ?? false,
		runInline: opts?.runInline ?? false,
		searchMode,
		atMode,
		completions: items,
		completionIndex: index,
		inputDecision,
		runDecision,
		hasAttachments: attachments.any,
	});

	await actuate(action);
}

/**
 * Actuate a decided SubmitAction — the side-effecting half of submit. Extracted
 * so BOTH the Enter path (handleSubmit) and the click path (handleCompletionSelect)
 * run the SAME reducer→actuator pipeline; the click path no longer re-derives
 * routing (correction/ask-ai/search-web/fill/run), which had already drifted from
 * this switch. Pure UI gestures unique to a CLICK (@/search-mode open vs Enter's
 * drill) stay in handleCompletionSelect and never reach here.
 */
async function actuate(action: SubmitAction): Promise<void> {
	switch (action.kind) {
		case "noop":
			return;

		case "panel": {
			inputValue = "";
			completions.items = [];
			completions.index = -1;
			// One call opens exactly one panel; any on-screen AI answer auto-parks.
			ui.openPanel(action.panel);
			if (action.panel === "notes") initialNotesTab = action.notesTab;
			if (action.panel === "chat-history") {
				getConversations().then((cs) => {
					conversations = cs;
				});
			}
			return;
		}

		case "reveal":
			revealSelected();
			return;

		case "completion-select":
			await handleCompletionSelect(action.label, action.ctrlKey);
			return;

		case "fill":
			// A tab-to-complete hint always fills — it's an incomplete command
			// waiting for an argument, so there is nothing to run yet.
			completions.items = [];
			completions.index = -1;
			inputValue = action.value;
			handleInput(inputValue);
			return;

		case "correct":
			completions.items = [];
			completions.index = -1;
			if (action.run) {
				// The user picked this row — run it now. Filling and making them
				// press Enter again is friction for a choice already made.
				inputValue = "";
				await runCommand(action.value);
				return;
			}
			// An auto-offered guess: fill and let the user confirm, so a wrong
			// correction never executes on its own.
			inputValue = action.value;
			handleInput(inputValue);
			return;

		case "calc-display":
			lastResult = {
				success: true,
				output: action.text,
				error: null,
				duration_ms: 0,
				auto_open: false,
			};
			inputValue = "";
			completions.items = [];
			completions.index = -1;
			return;

		case "open-path": {
			const opened = await openPath(action.path);
			if (opened) {
				inputValue = "";
				completions.items = [];
				completions.index = -1;
				cancelFileSearch();
				await hide();
			}
			return;
		}

		case "command":
			// An AI-chat recall row (from the empty-box recents) → open the chat,
			// not the executor.
			if (action.command.startsWith(CHAT_RUN_PREFIX)) {
				completions.items = [];
				completions.index = -1;
				await openConversation(action.command.slice(CHAT_RUN_PREFIX.length));
				return;
			}
			await runCommand(action.command, { runInline: action.runInline });
			return;

		case "agent": {
			inputValue = "";
			completions.items = [];
			completions.index = -1;
			lastResult = null; // AI answer takes over the result area
			let prompt = expandCopiedToken(action.prompt, submitPayload);
			// A demoted preset chain ("summarize and add it to notes") implies
			// material that is not inline: attach the PRIMARY selection inside
			// the same <pasted> identifier the copied-text token expands into —
			// the model treats it as material, the tool ranker ignores it.
			if (action.wantsSelection && submitPayload === null) {
				const sel = await readSelection().catch(() => null);
				if (sel) prompt = `${prompt}\n\n<pasted>\n${sel}\n</pasted>`;
			}
			await chat.start(prompt, /* fresh */ true);
			return;
		}

		case "preset": {
			// An AI command (preset). If the user typed no text after the keyword,
			// fall back to the PRIMARY selection (highlighted text in the focused
			// window) — so `summarize` alone acts on what you've selected in a
			// browser/editor. Runs through the ONE agent lane, tools included: a
			// pure transform just answers, and a chained ask ("…and add it to
			// notes") can act — no second engine, no routing dilemma.
			let input = expandCopiedToken(action.input, submitPayload);
			if (!input) {
				const sel = await readSelection().catch(() => null);
				if (sel) input = sel;
			}
			// Nothing to act on (no typed text, nothing selected): don't fire the
			// model on an empty payload — it can only answer "provide the text".
			// Re-fill the keyword and show its argument hint so the user can paste.
			if (!input) {
				inputValue = `${action.keyword} `;
				flashHint(`Type or select text for ${action.keyword}`);
				return;
			}
			inputValue = "";
			completions.items = [];
			completions.index = -1;
			lastResult = null; // AI answer takes over the result area
			// Material rides inside <pasted>: the model treats it as data, the
			// tool ranker ignores it, and injection hygiene comes free.
			const wrapped = input.includes("<pasted>") ? input : `<pasted>\n${input}\n</pasted>`;
			await chat.start(renderPreset(action.template, wrapped), /* fresh */ true, action.keyword);
			return;
		}

		case "ai-disabled": {
			// The user made an AI request but no provider is configured. Warn
			// (more prominently for an explicit `ask`), then run the backend's
			// chosen web-search fallback (`action.command`).
			inputValue = "";
			completions.items = [];
			completions.index = -1;
			const note = action.explicit
				? "AI is disabled — enable it in Settings. Searching the web instead."
				: "AI is disabled — searching the web instead.";
			lastResult = {
				success: true,
				output: note,
				error: null,
				duration_ms: 0,
				auto_open: false,
			};
			await runCommand(action.command);
			return;
		}
	}
}

/**
 * What the user had typed when the current submit began.
 *
 * Set ONCE, at the top of the submit flow, because the branches below clear
 * `inputValue` at different points — some before calling `runCommand`, some
 * after — so reading it later gives a different answer depending on which path
 * ran. Consumed and cleared by `runCommand`.
 *
 * Deliberately one variable rather than a `query` argument on all eight
 * `runCommand` call sites: an argument every caller must remember to pass is a
 * rule that lives in eight places, and the ones that forget fail silently — the
 * launcher just stops learning, with nothing to notice. This is the same
 * "one decider" shape as `suggestions::rank` on the backend.
 */
let pendingQuery: string | undefined;

/**
 * The staged clipboard payload snapshotted at submit. The exits (runCommand,
 * chat.start, presets) expand the [copied text] token through THIS, because by
 * the time they run the input has been cleared and the live payload dropped by
 * its lifecycle effect. Overwritten on every submit, so it can never leak a
 * stale payload into a later, unrelated submit.
 */
let submitPayload: string | null = null;

/** Begin a submit: record what was typed, for `runCommand` to learn from. */
function beginSubmit() {
	submitPayload = pastedPayload;
	// Learning uses the EXPANDED query — the token form would teach the
	// frecency store a placeholder no future input will ever match.
	pendingQuery = expandCopiedToken(inputValue.trim(), submitPayload) || undefined;
}

async function runCommand(command: string, opts?: { runInline?: boolean }) {
	if (isExecuting) return;
	// The [copied text] token expands to its staged payload at THIS choke point,
	// so every command path — Enter, row click, correction — sees the real text.
	command = expandCopiedToken(command, submitPayload);
	isExecuting = true;
	const generation = ++executeGeneration;
	completions.items = [];
	completions.index = -1;
	ui.showLauncher();
	try {
		lastCommand = command;
		// Screenshot commands open a region/window selector — Lychi must leave
		// the screen first or it lands in the shot and blocks the selector. Hide
		// and let the compositor unmap before the capture starts.
		if (/^screenshot(\s|$)/i.test(command.trim())) {
			await hide();
			await new Promise((r) => setTimeout(r, 180));
		}
		// Consume the query for this submit. Taken (not copied) so a later
		// programmatic `runCommand` — a panel action, a retry — cannot re-learn
		// from a query the user has moved on from.
		const query = pendingQuery;
		pendingQuery = undefined;
		// A command identical to what was typed teaches nothing: the query was
		// its own answer. Filtered HERE rather than at the call sites so the
		// rule holds for every path.
		const learnFrom = query && query !== command.trim() ? query : undefined;
		const result = await executeCommand(command, undefined, opts?.runInline, learnFrom);
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
		} else if (lastResult.output === PANEL.media) {
			ui.openPanel("media");
			lastResult = null;
		} else if (lastResult.output === PANEL.notes) {
			ui.openPanel("notes");
			lastResult = null;
		} else if (lastResult.output === PANEL.timer) {
			ui.openPanel("notes");
			initialNotesTab = "timers";
			lastResult = null;
		} else if (lastResult.output === PANEL.reminders) {
			ui.openPanel("notes");
			initialNotesTab = "reminders";
			lastResult = null;
		} else if (lastResult.output === PANEL.snippets) {
			ui.openPanel("notes");
			initialNotesTab = "snippets";
			lastResult = null;
		} else if (lastResult.output?.startsWith(PREFIX.notesLimit)) {
			pendingNoteText = lastResult.output.slice(PREFIX.notesLimit.length);
			ui.openPanel("notes");
			lastResult = null;
		} else if (lastResult.output?.startsWith(PREFIX.browsePanel)) {
			const dir = lastResult.output.slice(PREFIX.browsePanel.length);
			inputValue = `@${dir}`;
			completions.atMode = true;
			completions.atStart = 0;
			lastResult = null;
			fetchAtCompletions(dir);
		} else if (lastResult.success && !hasVisibleOutput(lastResult)) {
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
		// C6: a consent confirmation carries its TYPED feature key from the
		// backend (the one place that knows which gate fired). This used to be
		// recovered by substring-matching the prompt prose ("freeipapi.com" /
		// "ifconfig.me") — rewording a sentence silently broke persistence.
		if (lastResult.consent_feature) {
			await grantPrivacyConsent(lastResult.consent_feature);
		}

		// G1: execute the EXACT action that was assessed (stored backend-side),
		// not a re-resolve of the raw string — closes the confirmation TOCTOU gap.
		lastResult = await confirmExecution();
		historyEntries = [...historyEntries, lastCommand];
		inputValue = "";
		if (lastResult.open_url && (lastResult.auto_open || !lastResult.output)) {
			await hide();
			await openUri(lastResult.open_url);
		} else if (lastResult.success && !hasVisibleOutput(lastResult)) {
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
	completions.searchMode = false;
	completions.atMode = false;
	completions.atStart = -1;
	completions.items = [];
	completions.index = -1;
	cancelFileSearch();
	inputValue = "";
	await hide();
	await openUri(`file://${label}`);
}

/**
 * Stage the highlighted file-search / `@` result as an AI attachment (Ctrl+Shift+A).
 * The backend classifies it — this only forwards the path and clears the picker
 * so the user is left at an empty prompt with a chip, ready to type the ask.
 */
async function attachSelectedFile() {
	if (!(completions.searchMode || completions.atMode)) return;
	if (completions.items.length === 0 || completions.index < 0) return;
	const item = completions.items[completions.index];
	// Folders aren't attachable (the backend refuses them too) — drilling in is
	// the useful gesture there, so leave the picker alone.
	if (item.icon_path === "__folder__") return;

	completions.searchMode = false;
	completions.atMode = false;
	completions.atStart = -1;
	completions.items = [];
	completions.index = -1;
	cancelFileSearch();
	inputValue = "";
	await attachments.add([item.label]);
}

/** Drill into a folder result — browse its contents inside Lychi (Tab / →). */
function drillIntoFolder(item: CompletionItem) {
	// `@`-browse: keep the `@` prefix and put the folder's FULL path after it, so
	// re-running `@`-completion lists THAT folder (a path-like partial routes to
	// listPathCompletions). The label is already the full `~/…/` path, so drilling
	// a nested folder resolves correctly — the old code dropped the `@` and rebuilt
	// a `/`-search path, which resolved a nested folder like `BG` as `~/BG` and
	// showed "Empty folder".
	if (completions.atMode) {
		// Replace ONLY the `@…` token in place, preserving any text the user typed
		// before it (e.g. "translate this @~/Pictures/BG/" must keep "translate
		// this "). The token runs from `atStart` to the next whitespace (or end).
		const label = item.label.endsWith("/") ? item.label : `${item.label}/`;
		const start = completions.atStart >= 0 ? completions.atStart : inputValue.indexOf("@");
		const before = start > 0 ? inputValue.slice(0, start) : "";
		const afterToken = inputValue.slice(start + 1); // text after the `@`
		const wsIdx = afterToken.search(/\s/);
		const after = wsIdx === -1 ? "" : afterToken.slice(wsIdx); // trailing text (incl. its space)
		inputValue = `${before}@${label}${after}`;
		handleInput(inputValue);
		return;
	}
	// `/`-search: convert the label to a scope-relative `/path/`.
	let path = item.label;
	if (path.startsWith("~/")) {
		path = path.slice(2); // ~/Downloads/ → Downloads/
	} else if (completions.activeScope && path.startsWith(completions.activeScope)) {
		path = path.slice(completions.activeScope.length); // /mnt/DevSSD/Colris/ → Colris/
		if (path.startsWith("/")) path = path.slice(1);
	}
	// Prepend a single leading slash for scope-relative paths, but never double
	// it: an absolute label (that didn't match the scope-strip branch above)
	// already starts with `/`.
	inputValue = path.startsWith("/") ? path : `/${path}`;
	handleInput(inputValue);
}

/** Open the folder for a path an AI answer produced (the "Open folder" action,
 * and Enter when the follow-up box is empty). `reveal_path` expands a leading
 * `~` itself, so the answer's raw path string is passed straight through. */
async function revealAnswerPath(path: string) {
	await hide();
	await revealPath(path);
}

/** Reveal the selected search result in the file manager (Ctrl+Enter). */
async function revealSelected() {
	const item = completions.items[completions.index];
	if (!item) return;
	const full = resolveFullPath(item.label);
	if (!full) return;
	await hide();
	await revealPath(full);
}

/** Copy the selected search result's full path, flashing a confirmation. */
async function copySelectedPath() {
	const item = completions.items[completions.index];
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

// --- ⌘K action panel ---
// A secondary-actions menu for the selected result. The action IMPLEMENTATIONS
// already exist above; this just enumerates which apply (frontend-only, keyed off
// mode flags + icon_path sentinels + run/fill/label conventions) and dispatches.

type PanelAction = { id: string; label: string; hint?: string };

/** The selected completion, or undefined (skips separators just in case). */
let selectedCompletion = $derived.by(() => {
	const item = completions.items[completions.index];
	if (!item || item.icon_path === "__separator__") return undefined;
	return item;
});

/**
 * Is a modal awaiting a yes/no decision?
 *
 * ONE value for both prompts — the command confirm panel (`ResultPanel`) and
 * the AI tool-call approval (`AiAnswer`). They are separate components with
 * separate key handling, which is precisely how I-016 happened: the exemption
 * in `ALLOWED_SHARED_BINDINGS` that lets `approve_action` share Ctrl+Enter with
 * `web_search` was reasoned about for the AI prompt (which owns the keyboard)
 * and silently covered the confirm panel (which does not).
 *
 * Derived rather than passed per-modal: a prop each new modal must remember to
 * set is the kind that fails silently when the next one forgets.
 */
let decisionPending: boolean = $derived.by(() => {
	const r: CommandResult | null = lastResult;
	return !!r?.needs_confirmation || !!chat.approval;
});

/**
 * Tell the completions store when an output surface is on screen, so it can gate
 * the suggestion list. Mirrors the three template branches below (AI answer,
 * agent plan, result panel) — the surfaces that OWN the area under the input.
 *
 * An effect rather than a call at each execution site: every path that produces
 * output already sets one of these three, so deriving from them means a new
 * output path is covered the moment it renders. The alternative — clearing
 * `completions.items` wherever a command runs — is what we had, and a single
 * missed site (a recents row running a command, then `afterContextReady`
 * repopulating the list beneath the result) showed both surfaces stacked.
 */
$effect(() => {
	completions.outputShown = ui.aiVisible || !!lastResult;
});

/** Build the applicable actions for the currently-selected result. */
let panelActions = $derived.by((): PanelAction[] => {
	const item = selectedCompletion;
	if (!item) return [];
	const acts: PanelAction[] = [];
	const isFolder = item.icon_path === "__folder__";
	const hasPath =
		completions.searchMode || completions.atMode || completions.filePathMap.has(item.label);
	// App rows carry a typed `kind: "app"` from the launcher + recent-apps (the
	// unambiguous sources). Pins/history whose `run` merely starts with `open `
	// have no kind — those keep the string check as an honest fallback, since a
	// pin can be `open https://…` and shouldn't be typed as an app.
	const isApp =
		item.kind === "app" || item.run?.startsWith("appctl") || item.run?.startsWith("open ");
	const isCalc = item.kind === "calc";
	// Chat-recall has no backend row — the `__chat__:` run is assembled on the FE,
	// so this stays a sentinel check (there is no kind to read).
	const isAiChat = item.run?.startsWith(CHAT_RUN_PREFIX);

	if (isAiChat) {
		// An AI-chat recall row: open/continue it, or delete it.
		acts.push({ id: "open", label: "Open conversation", hint: getComboString("submit") });
		acts.push({ id: "ai_delete_chat", label: "Delete conversation" });
		return acts;
	}

	if (hasPath) {
		acts.push({
			id: "open",
			label: isFolder ? "Open folder" : "Open",
			hint: getComboString("submit"),
		});
		if (isFolder)
			acts.push({ id: "drill", label: "Browse inside", hint: getComboString("tab_complete") });
		// `reveal` is Ctrl+Enter in search mode — the same binding `web_search`
		// owns elsewhere, so read it from there rather than restating the string.
		acts.push({
			id: "reveal",
			label: "Reveal in file manager",
			hint: getComboString("web_search"),
		});
		acts.push({ id: "copy_path", label: "Copy path", hint: getComboString("copy_path") });
	} else if (isApp) {
		acts.push({ id: "open", label: "Open / focus", hint: getComboString("submit") });
	} else if (isCalc) {
		acts.push({ id: "copy_value", label: "Copy result", hint: getComboString("submit") });
	} else {
		// Generic: runnable and/or fillable.
		if (item.fill) {
			acts.push({
				id: "insert",
				label: "Insert into input",
				hint: getComboString("tab_complete"),
			});
		}
		if (item.run || !item.fill) {
			acts.push({ id: "open", label: "Run", hint: getComboString("submit") });
		}
		if (item.run) {
			acts.push({ id: "copy_command", label: "Copy command" });
		}
	}

	// Pin to the zero state — for any runnable row (same condition as "Run":
	// a fill-only hint has no command worth pinning). The verb switches on the
	// backend-stamped `pinned` flag (never re-derived from display text).
	if (!hasPath && !isCalc && (item.run || !item.fill)) {
		acts.push(item.pinned ? { id: "unpin", label: "Unpin" } : { id: "pin", label: "Pin to top" });
	}

	// AI actions available for any non-path row (ask the agent about it / open a
	// fresh chat seeded with the label). Only when AI is configured.
	if (aiEnabled && !hasPath && !isCalc) {
		acts.push({ id: "ai_ask", label: "Ask AI about this", hint: "" });
	}
	return acts;
});

/** The command a row actually executes — what a pin must store. `run` when the
 *  backend set one, else the label (the same fallback selection uses). */
function effectiveRun(item: CompletionItem): string {
	return item.run ?? item.label;
}

function openActionPanel() {
	// Toggle: pressing Ctrl+K again (or the affordance) closes an open panel.
	if (ui.actionPanelOpen) {
		closeActionPanel();
		return;
	}
	if (!selectedCompletion || panelActions.length === 0) return;
	ui.actionPanelOpen = true;
}

function closeActionPanel() {
	ui.actionPanelOpen = false;
	// Restore focus to the input (the panel had grabbed it).
	requestAnimationFrame(() => {
		document.querySelector<HTMLInputElement>(".input-container input")?.focus();
	});
}

async function copyTextFlash(text: string, msg: string) {
	try {
		await navigator.clipboard.writeText(text);
		flashHint(msg);
	} catch (err) {
		console.error("[action-panel] clipboard write failed:", err);
	}
}

function runPanelAction(id: string) {
	const item = selectedCompletion;
	ui.actionPanelOpen = false;
	if (!item) {
		closeActionPanel();
		return;
	}
	switch (id) {
		case "open":
			handleCompletionSelect(item.label);
			break;
		case "drill":
			drillIntoFolder(item);
			closeActionPanel();
			break;
		case "reveal":
			revealSelected();
			break;
		case "copy_path": {
			const full = resolveFullPath(item.label);
			if (full) copyTextFlash(full, "Path copied");
			closeActionPanel();
			break;
		}
		case "copy_value":
			// "= 1024" → copy "1024"
			copyTextFlash(item.label.replace(/^=\s*/, ""), "Result copied");
			closeActionPanel();
			break;
		case "copy_command":
			copyTextFlash(item.run ?? item.label, "Command copied");
			closeActionPanel();
			break;
		case "pin":
			addPin(effectiveRun(item), item.label)
				.then(() => {
					flashHint("Pinned");
					loadEmptySuggestions();
				})
				.catch((err) => flashHint(String(err?.message ?? err)));
			closeActionPanel();
			break;
		case "unpin":
			removePin(effectiveRun(item))
				.then(() => {
					flashHint("Unpinned");
					loadEmptySuggestions();
				})
				.catch((err) => flashHint(String(err?.message ?? err)));
			closeActionPanel();
			break;
		case "insert":
			if (item.fill) {
				inputValue = item.fill;
				handleInput(inputValue);
			}
			closeActionPanel();
			break;
		case "ai_ask":
			// Seed a fresh agent chat with the row's label.
			closeActionPanel();
			inputValue = "";
			completions.items = [];
			completions.index = -1;
			lastResult = null; // AI answer takes over the result area
			chat.start(expandCopiedToken(item.label, pastedPayload), /* fresh */ true);
			break;
		case "ai_delete_chat":
			// Delete a recalled conversation (AI-chat recent row).
			if (item.run?.startsWith(CHAT_RUN_PREFIX)) {
				deleteConversationEntry(item.run.slice(CHAT_RUN_PREFIX.length));
			}
			closeActionPanel();
			break;
		default:
			closeActionPanel();
	}
}

async function handleCompletionSelect(label: string, forceOpen?: boolean) {
	// Also an entry point: a CLICK on a row reaches here without passing through
	// `handleSubmit`. Calling it twice on the Enter path is harmless — both read
	// the same still-unchanged `inputValue`.
	beginSubmit();
	// An AI-chat recall row (empty-box recents) → open the conversation.
	const clicked = completions.items.find((c) => c.label === label);
	if (clicked?.run?.startsWith(CHAT_RUN_PREFIX)) {
		completions.items = [];
		completions.index = -1;
		openConversation(clicked.run.slice(CHAT_RUN_PREFIX.length));
		return;
	}
	// / search mode — Enter opens the folder/file; Ctrl+Enter (forceOpen) reveals
	// it in the file manager. Drilling into a folder is Tab / → (see drillIntoFolder).
	if (completions.searchMode) {
		if (forceOpen) {
			revealSelected();
		} else {
			openFileByLabel(label);
		}
		return;
	}

	// @ browse mode — reference a file in the command line
	if (completions.atMode) {
		// A context source (@clipboard / @selection) isn't a path: complete the
		// token in place and leave @ mode. The backend resolves it at send time.
		if (clicked?.icon_path === "__context__") {
			// A context source replaces the whole @-token with a bare label (no `@`,
			// trailing space), so it needs the token's `before`/`after`, not the
			// `@`-preserving splice.
			const { before, after } = atToken(inputValue, completions.atStart);
			inputValue = `${before}${label} ${after.trimStart()}`.trimEnd().concat(" ");
			completions.atMode = false;
			completions.atStart = -1;
			completions.items = [];
			completions.index = -1;
			return;
		}
		if (label.endsWith("/")) {
			// A folder: splice it in and keep browsing. Set input and immediately
			// fetch — handleInput will also fire but debounce means our call wins.
			inputValue = spliceAtToken(inputValue, completions.atStart, label);
			clearTimeout(completions.debounceTimer);
			fetchAtCompletions(label);
			requestAnimationFrame(() => {
				document.querySelector<HTMLInputElement>(".input-container input")?.focus();
			});
		} else {
			// A file: splice the reference in and leave @ mode.
			inputValue = spliceAtToken(inputValue, completions.atStart, label);
			completions.atMode = false;
			completions.atStart = -1;
			completions.items = [];
			completions.index = -1;
		}
		return;
	}

	// Normal (non-file-mode) selection: route through the SAME reducer + actuator
	// as Enter, with the clicked row FORCED as the selection. This is FE-1 — the
	// click path used to re-derive correction/ask-ai/search-web/fill/run handling
	// inline, which had already drifted from the Enter path (a recents row echoing
	// an NL query dead-ended on click but reached the agent on Enter). Now there is
	// one path: classify → decideSubmit → actuate.
	const item =
		completions.items.find((c) => c.label === label) ?? completions.items[completions.index];
	if (!item) {
		// No matching row (shouldn't happen) — fall back to opening the label.
		runCommand(`open ${label}`);
		return;
	}

	const trimmed = inputValue.trim();
	const selectedRun = item.run ?? undefined;
	// Classify the raw input AND the row's `run` (when it differs), exactly as the
	// Enter path does, so a replayed history/context command routes as if typed.
	const classify = (q: string) => classifyInput(q).catch(() => undefined);
	const [inputDecision, runDecision] = await Promise.all([
		trimmed ? classify(trimmed) : Promise.resolve(undefined),
		selectedRun && selectedRun !== trimmed ? classify(selectedRun) : Promise.resolve(undefined),
	]);

	const action = decideSubmit({
		trimmed,
		ctrlKey: forceOpen ?? false,
		runInline: false,
		// NOT in @/search mode here (handled above), so decideSubmit can never
		// return a `completion-select` — no recursion back into this function.
		searchMode: false,
		atMode: false,
		completions: [item],
		completionIndex: 0,
		inputDecision,
		runDecision,
		hasAttachments: attachments.any,
	});
	await actuate(action);
}

function handleScopeChange(index: number) {
	completions.scopeIndex = index;
	if (completions.searchMode && inputValue.startsWith("/")) {
		// Re-trigger search with new scope
		completions.searchDone = false;
		completions.fileSearchId++;
		const id = completions.fileSearchId;
		completions.items = [];
		completions.index = -1;
		completions.searchScopePath = "";

		const parsed = parseSearchInput(inputValue.slice(1));
		const baseScope = completions.mountPoints[index]?.path ?? "";
		const scope = searchScope(baseScope, parsed);
		// A folder-qualified search publishes its scope path (breadcrumb); a
		// top-level one leaves it empty, as before.
		if (parsed.hasFolder) completions.searchScopePath = scope;

		if (scope) startFileSearch(parsed.term, scope, id);
	}
}

function handleHistorySelect(entry: string) {
	inputValue = entry;
	ui.closePanel();
}

function handleToggleHistory() {
	ui.togglePanel("history");
	if (ui.panelVisible("history")) {
		completions.items = [];
		completions.index = -1;
	} else {
		// Re-fetch completions for current input
		handleInput(inputValue);
	}
}

function handleToggleChatHistory() {
	ui.togglePanel("chat-history");
	if (ui.panelVisible("chat-history")) {
		// Opening the recall list auto-parks any on-screen AI answer (ui machine).
		completions.items = [];
		completions.index = -1;
		getConversations().then((cs) => {
			conversations = cs;
		});
	} else {
		handleInput(inputValue);
	}
}

function handleToggleSettings() {
	ui.togglePanel("settings");
	if (ui.panelVisible("settings")) {
		completions.items = [];
		completions.index = -1;
	}
}

function handleToggleMedia() {
	ui.togglePanel("media");
	if (ui.panelVisible("media")) {
		completions.items = [];
		completions.index = -1;
	}
}

function handleToggleNotes() {
	ui.togglePanel("notes");
	if (ui.panelVisible("notes")) {
		completions.items = [];
		completions.index = -1;
	}
}

function handleShowResult() {
	// Close any panel back to the results surface (parked AI stays parked).
	ui.showLauncher();
	completions.items = [];
	completions.index = -1;
}

/**
 * Reflect the arrow-selected completion into the input box, so navigating a
 * suggestion (history, context, etc.) previews what Enter will run and lets the
 * user edit it — the Raycast/shell-history feel. Only fills rows that carry a
 * concrete command (`run`) or a plain label; skips fill-hints and info rows.
 * Search/@-browse modes are left alone (the input there is a path, not a query).
 */
function fillFromSelection() {
	if (completions.atMode || completions.searchMode) return;
	const sel = completions.items[completions.index];
	if (!sel || sel.icon_path === "__separator__" || sel.icon_path === "__info__") return;
	// A calc row ("= 42") or a correction row isn't a runnable command — don't
	// clobber the input with their display text.
	if (sel.kind === "correction" || sel.kind === "calc") return;
	const text = sel.run ?? sel.fill ?? sel.label;
	if (text) inputValue = text;
}

function handleArrowUp() {
	if (completions.items.length > 0) {
		let next = completions.index - 1;
		// Skip separator items
		while (next >= 0 && completions.items[next]?.icon_path === "__separator__") next--;
		// If we ran off the top onto (or past) a leading header, stay put rather
		// than landing on a non-selectable separator.
		if (next < 0 || completions.items[next]?.icon_path === "__separator__") return;
		completions.index = next;
		fillFromSelection();
	}
}

function handleArrowDown() {
	if (completions.items.length > 0) {
		let next = completions.index + 1;
		// Skip separator items
		while (
			next < completions.items.length &&
			completions.items[next]?.icon_path === "__separator__"
		)
			next++;
		completions.index = Math.min(completions.items.length - 1, next);
		fillFromSelection();
	}
}

/**
 * Run an action declared on a result row.
 *
 * The row supplied `{handler, id, target}` — never a command. The backend
 * resolves that triple against the handler that produced the row, so the
 * frontend cannot invent a verb or smuggle one through the target, and the
 * resolved command still passes through the executor's normal validation.
 */
async function handleRowAction(handler: string, id: string, target: string) {
	try {
		const result = await runRowAction(handler, id, target);
		lastResult = result;
		lastCommand = `${id} ${target}`;
	} catch (err) {
		uiLog.error(`[row-action] ${handler}/${id} failed: ${err}`);
	}
}

/**
 * Blank the surface, let one real paint commit while still mapped, THEN unmap —
 * the fix for the re-summon flash.
 *
 * The flash is WebKit's own compositor handing back its LAST rendered frame on
 * re-map (the previous session's content) before the reset DOM rasterizes. By
 * setting `launcherReady=false` (CSS `.not-ready{opacity:0}` → the card goes
 * transparent) and waiting for a real paint cycle BEFORE `hideWindow()`, WebKit's
 * last-held buffer becomes the transparent launcher, so the first post-map frame
 * is clean. The double rAF fires here because the window is still mapped (a
 * hidden window's frame clock is stopped — that's why the reset can't be done
 * after hide). The ONE hide primitive; every dismiss path routes through it.
 */
async function blankThenHide() {
	launcherReady = false;
	await new Promise<void>((resolve) => {
		requestAnimationFrame(() => requestAnimationFrame(() => resolve()));
	});
	await hideWindow();
}

async function hide() {
	// Logged because this is the OTHER way the window closes, and it used to be
	// silent. The backend records `[hide] window hidden`, but with no cause
	// attached an Escape press and a blur-dismiss are indistinguishable in the
	// log — which is exactly the ambiguity that made "it vanished while I typed"
	// take so long to pin down. `handleBlurDismiss` names itself; so should this.
	uiLog.info("[hide] user dismissed (escape/keybinding)");
	await blankThenHide();
}

// Quick screenshot trigger (bound to the `screenshot` keybinding). Lychi must
// leave the screen before the capture starts, or it lands in the shot — so we
// hide first, wait a beat for the compositor to actually unmap the surface,
// then fire `screenshot area`. Runs via executeCommand directly (not
// runCommand) because the window is already hidden — no panel/history churn.
async function quickScreenshot() {
	inputValue = "";
	completions.items = [];
	completions.index = -1;
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
	// AI chat Esc ladder: (1) a live stream → cancel it (keep the answer);
	// (2) an on-screen answer → PARK it (hide to the status-bar recall icon and
	// return to the launcher — nothing is lost; click the icon or run a new query
	// to bring it back). The answer is only truly cleared by a new query or a
	// re-summon (chat.reset). This keeps the fast-and-forget feel without losing an
	// answer to an accidental keypress.
	if (chat.streaming) {
		chat.cancel();
		return;
	}
	if (ui.aiVisible) {
		ui.parkAi();
		inputValue = "";
		return;
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
			<OnboardingBanner
				mode={hotkeyBannerMode}
				text={hotkeyBannerText}
				copied={hotkeyBannerCopied}
				oncopy={copyToggleCommand}
				onopensettings={openSettingsFromBanner}
				ondismiss={dismissHotkeyBanner}
			/>
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
			disabled={isExecuting}
			{decisionPending}
			executing={isExecuting}
			contextPill={context.pill}
			contextLoading={context.loading}
			atMode={completions.atMode}
			atStart={completions.atStart}
			searchMode={completions.searchMode}
			{aiEnabled}
			scopeCount={completions.mountPoints.length}
			ontabscope={() => handleScopeChange((completions.scopeIndex + 1) % completions.mountPoints.length)}
			ontabcomplete={() => {
				if (completions.items.length > 0 && completions.index >= 0) {
					const item = completions.items[completions.index];
					// Standard file-picker keys (VS Code + Raycast + shell): Tab and
					// → DRILL into a folder (in both `/`-search and `@`-browse); Tab
					// on a file accepts it. Enter is pure accept (handled elsewhere) —
					// it never drills, so a folder can still be selected as-is.
					if (item.icon_path === "__folder__") {
						drillIntoFolder(item);
					} else if (completions.searchMode) {
						// File in search mode — open it.
						openFileByLabel(item.label);
					} else {
						// File in `@`-browse — insert the reference.
						handleCompletionSelect(item.label);
					}
				}
			}}
			ondrillinto={() => {
				// Arrow-right → drill into the selected folder, in BOTH search and
				// `@`-browse modes (Alfred convention).
				if (
					(completions.searchMode || completions.atMode) &&
					completions.items.length > 0 &&
					completions.index >= 0
				) {
					const item = completions.items[completions.index];
					if (item?.icon_path === "__folder__") drillIntoFolder(item);
				}
			}}
			oncopypath={() => {
				if (completions.searchMode && completions.items.length > 0 && completions.index >= 0) {
					copySelectedPath();
				}
			}}
			onshifttabback={() => {
				if (completions.searchMode && inputValue.startsWith("/")) {
					// Go up a directory level (parentSearchInput handles the trailing
					// slash + root stop; see modes.ts).
					inputValue = parentSearchInput(inputValue.slice(1));
					handleInput(inputValue);
				} else if (completions.atMode && completions.atStart >= 0) {
					// Parent of the current @-partial, spliced back into its token.
					const parent = parentAtPartial(atPartial(inputValue, completions.atStart));
					inputValue = spliceAtToken(inputValue, completions.atStart, parent);
					clearTimeout(completions.debounceTimer);
					fetchAtCompletions(parent);
				}
			}}
			searchGhost={completions.searchMode && completions.items.length > 0 && completions.index >= 0 ? completions.items[completions.index].label : ""}
			browseGhost={completions.atMode && completions.items.length > 0 && completions.index >= 0 ? completions.items[completions.index].label : ""}
			history={historyEntries}
			onactionpanel={openActionPanel}
			onattachfile={attachSelectedFile}
			onpastefiles={attachments.addFromClipboard}
			onpastetext={stagePastedText}
			pastedStaged={pastedPayload !== null}
			onremovelastattachment={attachments.removeLast}
			hasAttachments={attachments.any}
		/>
		<AttachmentTray />
		{#if ui.actionPanelOpen}
			<ActionPanel
				actions={panelActions}
				onrun={runPanelAction}
				ondismiss={closeActionPanel}
			/>
		{/if}
		<!-- Panels: mounted only ONCE the launcher is ready, so nothing but the
		     launcher (input + suggestions + status bar) exists in the DOM on the
		     first paint / re-summon. They were previously always-mounted and
		     merely CSS-hidden, which flashed on load before the reset committed.
		     Guarding on `launcherReady` costs the first panel-open a one-time mount
		     (negligible) and eliminates the flash entirely; a panel still toggles
		     instantly after that first mount. Exactly one is ever visible (the `ui`
		     surface machine makes overlap unrepresentable). -->
		{#if launcherReady}
			<div class:panel-hidden={!ui.panelVisible("history")}>
				<HistoryPanel entries={historyEntries} onselect={handleHistorySelect} />
			</div>
			<div class:panel-hidden={!ui.panelVisible("chat-history")}>
				<ChatHistoryPanel
					{conversations}
					visible={ui.panelVisible("chat-history")}
					onselect={openConversation}
					ondelete={deleteConversationEntry}
					onclear={clearAllConversations}
					ondismiss={ui.closePanel}
				/>
			</div>
			<div class:panel-hidden={!ui.panelVisible("notes")}>
				<NotesPanel ondismiss={() => { ui.closePanel(); pendingNoteText = null; initialNotesTab = undefined; }} {pendingNoteText} onpendingcleared={() => { pendingNoteText = null; }} {initialNotesTab} visible={ui.panelVisible("notes")} />
			</div>
			<div class:panel-hidden={!ui.panelVisible("media")}>
				<MediaPanel visible={ui.panelVisible("media")} ondismiss={ui.closePanel} players={media.players} />
			</div>
			<div class="settings-wrapper" class:panel-hidden={!ui.panelVisible("settings")}>
				<SettingsPanel bind:this={settingsPanel} ondismiss={ui.closePanel} />
			</div>
		{/if}
		{#if ui.aiVisible}
			<AiAnswer
				bind:this={aiAnswerRef}
				turns={chat.turns}
				lastUser={chat.lastUser}
				lastAttachment={chat.lastAttachment}
				lastFiles={chat.lastFiles}
				text={chat.text}
				truncated={chat.truncated}
				tokensIn={chat.tokensIn}
				tokensOut={chat.tokensOut}
				tokensCached={chat.tokensCached}
				streaming={chat.streaming}
				notice={chat.notice}
				noticeDeadline={chat.noticeDeadline}
				error={chat.error}
				toolSteps={chat.toolSteps}
				approval={chat.approval}
				onapprove={chat.approve}
				bind:reply={chat.reply}
				onreply={chat.sendReply}
				onstop={chat.cancel}
				onregenerate={chat.regenerate}
				onreveal={revealAnswerPath}
				resumed={chat.resumed}
				onstartfresh={startFreshFromResume}
			/>
		{:else if !ui.anyPanelOpen}
			<CompletionsList
				items={completions.visible ? completions.items : []}
				selectedIndex={completions.index}
				onselect={handleCompletionSelect}
				pathContext={completions.searchMode ? searchPathContext : (completions.atMode ? atPathContext : "")}
				scopeTabs={completions.searchMode && completions.mountPoints.length > 1 ? completions.mountPoints : []}
				activeScopeIndex={completions.scopeIndex}
				onscopechange={handleScopeChange}
				searching={completions.searchMode && !completions.searchDone}
				browseMode={completions.atMode}
				searchMode={completions.searchMode}
				metaMap={completions.searchMode ? completions.fileMetaMap : undefined}
				ignoreActive={completions.searchMode && completions.ignoreActive}
				{flashMessage}
			/>
			{#if completions.items.length === 0 && completions.atMode && completions.atNoResults}
				<div class="empty-state">Empty folder</div>
			{:else if completions.items.length === 0 && completions.searchMode && !completions.searchDone}
				<!-- Only reachable with genuinely nothing to show: a cold first query,
				     or a scope whose walk hasn't produced matches yet. Keystrokes no
				     longer clear `items`, so this can't flash between result sets
				     anymore — it used to, once per keypress. -->
				<div class="empty-state">Searching...</div>
			{:else if completions.items.length === 0 && completions.pending && !completions.atMode && !completions.searchMode && inputValue.trim().length > 0}
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
				onopenfile={(path) => runCommand(`file ${path}`)}
				onrowaction={handleRowAction} />
			{/if}
		{/if}
		<StatusBar
			result={lastResult}
			executing={isExecuting}
			historyOpen={ui.panelVisible("history")}
			settingsOpen={ui.panelVisible("settings")}
			mediaOpen={ui.panelVisible("media")}
			chatHistoryOpen={ui.panelVisible("chat-history")}
			notesOpen={ui.panelVisible("notes")}
			aiParked={ui.aiParked}
			nowPlaying={media.nowPlaying}
			multiplePlayers={media.multiplePlayers}
			ontogglehistory={handleToggleHistory}
			ontogglesettings={handleToggleSettings}
			ontogglemedia={handleToggleMedia}
			ontogglenotes={handleToggleNotes}
			ontogglechathistory={handleToggleChatHistory}
		onshowresult={handleShowResult}
		onshowai={ui.restoreAi}
		contextStale={context.stale}
		contextStaleHint={context.staleHint}
		contextRefreshing={context.refreshing}
		aiLoading={ui.aiLoading}
		actionsAvailable={!ui.anyPanelOpen && panelActions.length > 0}
		onactionpanel={openActionPanel}
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

	main {
		width: 680px;
		max-width: calc(100vw - 40px);
		max-height: 60vh;
		display: flex;
		flex-direction: column;
		/* Positioning context for the ⌘K action panel, which anchors to the
		   launcher's bottom-right rather than the viewport. */
		position: relative;
		/* The card surface. Its opacity is user-configurable (--card-opacity, 0–1;
		   default 1 = fully opaque) via color-mix, and its corner radius via
		   --card-radius (default 12px). The child surfaces (input bar, panels,
		   status bar) are transparent and show THIS background, so one translucent
		   layer reads uniformly instead of stacking opacities. */
		background: color-mix(
			in srgb,
			var(--bg) calc(var(--card-opacity, 1) * 100%),
			transparent
		);
		border: 1px solid var(--border);
		border-radius: var(--card-radius, 12px);
		overflow: hidden;
		box-shadow: 0 8px 32px var(--shadow-overlay);
		animation: lychi-appear 120ms ease-out;
	}
	/* Frosted-glass fallback (--card-frost = 1). Real desktop blur is requested
	   from the compositor (KWin) in the backend; where that isn't available this
	   adds a faint light wash + inset highlight so "frost" still reads visually.
	   A CSS `backdrop-filter` here would be a no-op (it can't see the desktop
	   behind a transparent webview), so this is a tint, not real blur. */
	main::before {
		content: "";
		position: absolute;
		inset: 0;
		pointer-events: none;
		opacity: var(--card-frost, 0);
		transition: opacity 150ms ease;
		background: linear-gradient(
			135deg,
			color-mix(in srgb, var(--fg) 6%, transparent),
			transparent 60%
		);
		box-shadow: inset 0 1px 0 color-mix(in srgb, var(--fg) 10%, transparent);
		border-radius: inherit;
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

<script lang="ts">
import { onMount } from "svelte";
import AgentPlanPanel from "$lib/components/AgentPlanPanel.svelte";
import CommandInput from "$lib/components/CommandInput.svelte";
import CompletionsList from "$lib/components/CompletionsList.svelte";
import HistoryPanel from "$lib/components/HistoryPanel.svelte";
import MediaPanel from "$lib/components/MediaPanel.svelte";
import NotesPanel from "$lib/components/NotesPanel.svelte";
import ResultPanel from "$lib/components/ResultPanel.svelte";
import SettingsPanel from "$lib/components/SettingsPanel.svelte";
import StatusBar from "$lib/components/StatusBar.svelte";
import type { AgentPlan, CommandResult, CompletionItem, StepEvent, TrackInfo } from "$lib/ipc";
import {
	executeCommand,
	getAgentPlan,
	getCompletions,
	getHideOnBlur,
	getHistory,
	hideWindow,
	listPathCompletions,
	mediaGetStatus,
	openUri,
	saveWindowPosition,
} from "$lib/ipc";

let inputValue = $state("");
let isExecuting = $state(false);
let isRouting = $state(false);
let lastResult: CommandResult | null = $state(null);
let lastCommand = $state("");

let historyEntries: string[] = $state([]);
let historyOpen = $state(false);

let completions: CompletionItem[] = $state([]);
let completionIndex = $state(-1);
let debounceTimer: ReturnType<typeof setTimeout> | undefined;

// @ file reference mode
let atMode = $state(false);
let atStart = $state(-1);

let settingsOpen = $state(false);
let hideOnBlur = $state(true);
let blurEnabled = $state(false);

let pendingPlan: AgentPlan | null = $state(null);
let planPanelRef: AgentPlanPanel | undefined = $state(undefined);
let mediaOpen = $state(false);
let notesOpen = $state(false);
let pendingNoteText: string | null = $state(null);
let mediaPlayers: TrackInfo[] = $state([]);
let mediaPollTimer: ReturnType<typeof setInterval> | undefined;

// Derive the "now playing" track — first playing, or first in list
let nowPlaying = $derived.by(() => {
	const playing = mediaPlayers.find((p) => p.status === "playing");
	return playing ?? mediaPlayers[0] ?? null;
});

let multiplePlayers = $derived(mediaPlayers.length > 1);

$effect(() => {
	getHistory().then((entries) => {
		historyEntries = entries;
	});
});

// Poll media player status (every 5s)
$effect(() => {
	function poll() {
		mediaGetStatus()
			.then((players) => {
				mediaPlayers = players.filter((p) => p.title || p.status !== "stopped");
			})
			.catch(() => {
				mediaPlayers = [];
			});
	}
	poll();
	mediaPollTimer = setInterval(poll, 5000);
	return () => clearInterval(mediaPollTimer);
});

// Called by CommandInput on every keystroke
function handleInput(val: string) {
	historyOpen = false;
	clearTimeout(debounceTimer);

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
			debounceTimer = setTimeout(async () => {
				try {
					const results = await listPathCompletions(partial);
					completions = results;
					completionIndex = results.length > 0 ? 0 : -1;
					// No results and not a path prefix — exit @ mode, fall back to normal
					if (results.length === 0 && partial.length > 0) {
						atMode = false;
						atStart = -1;
					}
				} catch (err) {
					console.error("[@completions] error:", err);
					atMode = false;
					atStart = -1;
				}
			}, 80);
			return;
		}
	}

	// Not in @ mode — normal completions
	atMode = false;
	atStart = -1;

	const trimmed = val.trim();
	if (trimmed.length < 2) {
		completions = [];
		completionIndex = -1;
		return;
	}

	debounceTimer = setTimeout(async () => {
		// Send raw input — the backend router handles intent detection
		try {
			const results = await getCompletions(trimmed);
			completions = results;
			completionIndex = results.length > 0 ? 0 : -1;
		} catch (err) {
			console.error("[completions] error:", err);
		}
	}, 80);
}

onMount(() => {
	// Load config
	getHideOnBlur().then((v) => {
		hideOnBlur = v;
	});

	// Guard: only attach Tauri listeners if running inside Tauri
	if (!("__TAURI_INTERNALS__" in window)) return;

	// Grace period — ignore blur events during startup (WMs can fire blur during show)
	setTimeout(() => {
		blurEnabled = true;
	}, 500);

	let unlisteners: (() => void)[] = [];

	(async () => {
		const { getCurrentWindow } = await import("@tauri-apps/api/window");
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
	if (!trimmed || isExecuting) return;

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
			// @ mode — drill into directory or open file
			if (atMode) {
				handleCompletionSelect(selected.label);
				return;
			}
			// Check if input has an explicit prefix (e.g. "spotify ", "system ", "media ")
			// If so, append the selected completion to the prefix
			const spaceIdx = trimmed.indexOf(" ");
			if (spaceIdx !== -1) {
				const prefix = trimmed.slice(0, spaceIdx);
				await runCommand(`${prefix} ${selected.label}`);
			} else {
				// No prefix — these are app completions, launch via open
				await runCommand(`open ${selected.label}`);
			}
			return;
		}
	}

	// Try AI plan first — if it returns a multi-step plan, show preview
	isRouting = true;
	try {
		const plan = await getAgentPlan(trimmed);
		if (plan) {
			pendingPlan = plan;
			completions = [];
			completionIndex = -1;
			historyOpen = false;
			return;
		}
	} catch (err) {
		console.error("[agent plan] error:", err);
	} finally {
		isRouting = false;
	}

	await runCommand(trimmed);
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
			handleInput(inputValue);
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

function handleCompletionSelect(label: string) {
	if (atMode) {
		// If it's a directory (ends with /), keep @ mode active for drilling down
		if (label.endsWith("/")) {
			const before = inputValue.slice(0, atStart);
			const afterAt = inputValue.slice(atStart);
			const spaceIdx = afterAt.indexOf(" ", 1); // skip the @ itself
			const after = spaceIdx === -1 ? "" : afterAt.slice(spaceIdx);
			inputValue = `${before}@${label}${after}`;
			// Re-trigger completions for the directory contents
			handleInput(inputValue);
			requestAnimationFrame(() => {
				document.querySelector<HTMLInputElement>(".input-container input")?.focus();
			});
		} else {
			// It's a file — open it directly
			atMode = false;
			atStart = -1;
			completions = [];
			completionIndex = -1;
			runCommand(`file ${label}`);
		}
		return;
	}
	runCommand(`open ${label}`);
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
	if (completions.length > 0) {
		completions = [];
		completionIndex = -1;
		atMode = false;
		atStart = -1;
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
			history={historyEntries}
		/>
		{#if settingsOpen}
			<SettingsPanel ondismiss={() => { settingsOpen = false; }} />
		{:else if mediaOpen}
			<MediaPanel ondismiss={() => { mediaOpen = false; }} players={mediaPlayers} />
		{:else if notesOpen}
			<NotesPanel ondismiss={() => { notesOpen = false; pendingNoteText = null; }} {pendingNoteText} onpendingcleared={() => { pendingNoteText = null; }} />
		{:else if pendingPlan}
			<AgentPlanPanel
				bind:this={planPanelRef}
				plan={pendingPlan}
				onexecuted={() => {
					historyEntries = [...historyEntries, pendingPlan!.input];
					inputValue = "";
				}}
				ondismiss={() => { pendingPlan = null; }}
			/>
		{:else}
			{#if completions.length > 0}
				<CompletionsList
					items={completions}
					selectedIndex={completionIndex}
					onselect={handleCompletionSelect}
				/>
			{/if}
			{#if historyOpen}
				<HistoryPanel entries={historyEntries} onselect={handleHistorySelect} />
			{:else if lastResult}
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
		align-self: flex-start;
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

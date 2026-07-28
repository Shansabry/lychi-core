<script lang="ts">
import { RotateCcw } from "lucide-svelte";
import type { KeybindingsConfig } from "$lib/ipc";
import { KEYBINDINGS_DEFAULTS, saveKeybindingsConfig } from "$lib/ipc";
import {
	ACTION_LABELS,
	type ActionId,
	ALL_ACTIONS,
	comboFromEvent,
	findConflicts,
	loadKeybindings,
} from "$lib/keybindings";
import { invalidateSettings } from "$lib/preloadCache";

let {
	keybindingsConfig = $bindable(),
}: {
	keybindingsConfig: KeybindingsConfig;
} = $props();

let recordingAction: ActionId | null = $state(null);
let conflictWarning = $state("");

function startRecording(action: ActionId) {
	recordingAction = action;
	conflictWarning = "";

	const handler = (e: KeyboardEvent) => {
		e.preventDefault();
		e.stopPropagation();

		const combo = comboFromEvent(e);
		if (!combo) return;

		if (e.key === "Escape" && !e.ctrlKey && !e.shiftKey && !e.altKey) {
			recordingAction = null;
			window.removeEventListener("keydown", handler, true);
			return;
		}

		const testConfig = { ...keybindingsConfig, [action]: combo };
		const conflicts = findConflicts(testConfig);
		if (conflicts.length > 0) {
			const [a, b] = conflicts[0];
			const other = a === action ? b : a;
			conflictWarning = `"${combo}" conflicts with ${ACTION_LABELS[other]}`;
		} else {
			conflictWarning = "";
		}

		keybindingsConfig = { ...keybindingsConfig, [action]: combo };
		loadKeybindings(keybindingsConfig);
		saveKeybindingsConfig(keybindingsConfig);
		invalidateSettings();

		recordingAction = null;
		window.removeEventListener("keydown", handler, true);
	};

	window.addEventListener("keydown", handler, true);
}

async function resetAllShortcuts() {
	keybindingsConfig = { ...KEYBINDINGS_DEFAULTS };
	loadKeybindings(keybindingsConfig);
	await saveKeybindingsConfig(keybindingsConfig);
	invalidateSettings();
	conflictWarning = "";
}
</script>

<div class="section-label first">In-app keys</div>
{#each ALL_ACTIONS as action (action)}
	<div class="field shortcut-row">
		<span class="field-label">{ACTION_LABELS[action]}</span>
		<button
			class="hotkey-btn"
			class:recording={recordingAction === action}
			onclick={() => startRecording(action)}
		>
			{#if recordingAction === action}
				Press keys...
			{:else}
				{keybindingsConfig[action]}
			{/if}
		</button>
	</div>
{/each}
{#if conflictWarning}
	<div class="field-error">{conflictWarning}</div>
{/if}
<div class="field" style="justify-content: flex-end; padding-top: 8px;">
	<button class="reset-btn" onclick={resetAllShortcuts}>
		<RotateCcw size={12} strokeWidth={1.5} />
		Reset all to defaults
	</button>
</div>

<style>
	@import "./rows.css";

	.shortcut-row {
		display: flex;
		align-items: center;
		justify-content: space-between;
	}

	.shortcut-row .hotkey-btn {
		min-width: 110px;
	}

	.hotkey-btn {
		background: var(--bg-secondary);
		color: var(--fg);
		border: 1px solid var(--border);
		border-radius: 4px;
		padding: 5px 12px;
		font-family: var(--font-mono);
		font-size: 12px;
		min-width: 140px;
		text-align: center;
		cursor: pointer;
		transition: border-color 100ms ease;
	}

	.hotkey-btn:hover {
		border-color: var(--fg-muted);
	}

	.hotkey-btn.recording {
		border-color: var(--accent);
		color: var(--accent);
		animation: hotkey-pulse 1s ease-in-out infinite;
	}

	@keyframes hotkey-pulse {
		0%, 100% { opacity: 1; }
		50% { opacity: 0.4; }
	}

	.reset-btn {
		display: flex;
		align-items: center;
		gap: 5px;
		background: var(--bg-secondary);
		color: var(--fg-muted);
		border: 1px solid var(--border);
		border-radius: 4px;
		padding: 4px 10px;
		font-size: 11px;
		cursor: pointer;
		transition: color 100ms ease, border-color 100ms ease;
	}

	.reset-btn:hover {
		color: var(--fg);
		border-color: var(--fg-muted);
	}
</style>

<script lang="ts">
import type { AiConfig } from "$lib/ipc";
import CommandsView from "./CommandsView.svelte";
import ConnectionView from "./ConnectionView.svelte";

let {
	aiConfig = $bindable(),
	onsaveerror,
	onpresetchange,
}: {
	aiConfig: AiConfig;
	onsaveerror: (msg: string) => void;
	/** Called when the user adds/edits/deletes a preset (launcher reloads them). */
	onpresetchange?: () => void;
} = $props();

let activeView: "connection" | "commands" = $state("connection");
let presetCount = $state(0);

let connectionRef: ConnectionView | undefined = $state();
let commandsRef: CommandsView | undefined = $state();

// SettingsPanel drives these via bind:this — the contract MUST be preserved.
export async function initModels(aiMode: string) {
	await connectionRef?.initModels(aiMode);
}

export function dismissConfirm(): boolean {
	// Whichever view owns a live confirm gets first crack.
	if (activeView === "connection") return connectionRef?.dismissConfirm() ?? false;
	return commandsRef?.dismissConfirm() ?? false;
}

function inField(e: KeyboardEvent): boolean {
	const t = e.target;
	return t instanceof HTMLInputElement || t instanceof HTMLTextAreaElement;
}

function handleKeydown(e: KeyboardEvent) {
	const cmdOrCtrl = e.metaKey || e.ctrlKey;

	// Sub-view switch — digits only when NOT typing in a field.
	if (!inField(e) && !cmdOrCtrl && !e.altKey) {
		if (e.key === "1" || e.key === "[") {
			activeView = "connection";
			e.stopPropagation();
			return;
		}
		if (e.key === "2" || e.key === "]") {
			activeView = "commands";
			e.stopPropagation();
			return;
		}
	}

	if (activeView === "connection") {
		if (cmdOrCtrl && e.key === "Enter") {
			e.preventDefault();
			connectionRef?.runConnectionTest();
			return;
		}
		if (e.altKey && (e.key === "a" || e.key === "A")) {
			e.preventDefault();
			connectionRef?.toggleAdvanced();
			return;
		}
		// Local-model list navigation — never while typing in a field.
		if (!inField(e)) {
			if (e.key === "ArrowDown") {
				e.preventDefault();
				connectionRef?.moveModelSelection(1);
				return;
			}
			if (e.key === "ArrowUp") {
				e.preventDefault();
				connectionRef?.moveModelSelection(-1);
				return;
			}
			if (e.key === "Enter") {
				e.preventDefault();
				connectionRef?.activateModel();
				return;
			}
			if (e.key === "Backspace") {
				e.preventDefault();
				connectionRef?.removeModel();
				return;
			}
		}
	} else {
		if (cmdOrCtrl && e.key === "Enter") {
			e.preventDefault();
			commandsRef?.save();
			return;
		}
		if (!inField(e)) {
			if (e.key === "ArrowDown") {
				e.preventDefault();
				commandsRef?.moveSelection(1);
				return;
			}
			if (e.key === "ArrowUp") {
				e.preventDefault();
				commandsRef?.moveSelection(-1);
				return;
			}
			if (e.key === "Backspace") {
				e.preventDefault();
				commandsRef?.deleteSelected();
				return;
			}
		}
	}
}
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
<div class="ai-tab" onkeydown={handleKeydown} tabindex={0}>
	<div class="subnav">
		<button
			class="seg"
			class:active={activeView === "connection"}
			onclick={() => (activeView = "connection")}
		>
			Connection
		</button>
		<button
			class="seg"
			class:active={activeView === "commands"}
			onclick={() => (activeView = "commands")}
		>
			Commands
			{#if presetCount > 0}<span class="count">{presetCount}</span>{/if}
		</button>
	</div>

	<div class="view">
		<!-- Both views stay mounted so refs + listeners survive tab switches. -->
		<div class:hidden={activeView !== "connection"}>
			<ConnectionView bind:this={connectionRef} bind:aiConfig {onsaveerror} />
		</div>
		<div class:hidden={activeView !== "commands"}>
			<CommandsView
				bind:this={commandsRef}
				{onpresetchange}
				oncount={(n) => (presetCount = n)}
			/>
		</div>
	</div>

	<div class="keyhints">
		{#if activeView === "connection"}
			<span class="kh"><span class="kbd">1</span><span class="kbd">2</span> switch view</span>
			<span class="kh"><span class="kbd">⌘↵</span> test connection</span>
			<span class="kh"><span class="kbd">⌥A</span> advanced</span>
			<span class="kh"><span class="kbd">Esc</span> close</span>
		{:else}
			<span class="kh"><span class="kbd">↑</span><span class="kbd">↓</span> navigate</span>
			<span class="kh"><span class="kbd">↵</span> edit / new</span>
			<span class="kh"><span class="kbd">⌘↵</span> save</span>
			<span class="kh"><span class="kbd">⌫</span> delete</span>
			<span class="kh"><span class="token">{"{input}"}</span> user's text</span>
		{/if}
	</div>
</div>

<style>
	.ai-tab {
		display: flex;
		flex-direction: column;
		min-height: 0;
		outline: none;
	}
	.subnav {
		display: flex;
		align-items: center;
		gap: 2px;
		border-bottom: 1px solid color-mix(in srgb, var(--border) 60%, transparent);
		margin-bottom: 16px;
	}
	.seg {
		position: relative;
		padding: 8px 14px 12px;
		font-size: 12.5px;
		color: var(--fg-muted);
		cursor: pointer;
		background: none;
		border: none;
		font-family: var(--font-mono);
		display: flex;
		align-items: center;
		gap: 7px;
	}
	.seg.active {
		color: var(--fg);
	}
	.seg.active::after {
		content: "";
		position: absolute;
		left: 10px;
		right: 10px;
		bottom: -1px;
		height: 2px;
		background: var(--accent);
		border-radius: 2px;
	}
	.seg .count {
		font-family: var(--font-mono);
		font-size: 10px;
		color: var(--fg-muted);
		background: var(--bg-secondary);
		border-radius: 10px;
		padding: 0 6px;
	}
	.view {
		flex: 1;
		min-height: 0;
	}
	.hidden {
		display: none;
	}
	.keyhints {
		display: flex;
		flex-wrap: wrap;
		gap: 6px 14px;
		margin-top: 18px;
		padding-top: 12px;
		border-top: 1px solid color-mix(in srgb, var(--border) 60%, transparent);
		color: var(--fg-muted);
		font-size: 11px;
		align-items: center;
	}
	.kh {
		display: inline-flex;
		align-items: center;
		gap: 5px;
	}
	.kbd {
		font-family: var(--font-mono);
		font-size: 10px;
		color: var(--fg-muted);
		background: var(--bg-secondary);
		border: 1px solid var(--border);
		border-radius: 4px;
		padding: 1px 5px;
	}
	.token {
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--accent);
	}
</style>

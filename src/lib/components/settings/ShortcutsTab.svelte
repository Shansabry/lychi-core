<script lang="ts">
import type { CommandsConfig, KeybindingsConfig } from "$lib/ipc";
import AliasesView from "./shortcuts/AliasesView.svelte";
import CliView from "./shortcuts/CliView.svelte";
import KeysView from "./shortcuts/KeysView.svelte";
import QuicklinksView from "./shortcuts/QuicklinksView.svelte";

let {
	keybindingsConfig = $bindable(),
	commandsConfig = $bindable(),
	onsaveerror,
}: {
	keybindingsConfig: KeybindingsConfig;
	commandsConfig: CommandsConfig;
	onsaveerror: (msg: string) => void;
} = $props();

// Four unrelated things used to share one scrolling page: key bindings, the CLI
// reference, parameterized quicklinks, and aliases. Splitting them into views
// keeps each legible; the sub-nav mirrors the AI tab so Settings has one
// navigation idiom rather than two.
type View = "keys" | "cli" | "quicklinks" | "aliases";
let activeView = $state<View>("keys");

let quicklinkCount = $state(0);
let aliasCount = $state(0);
</script>

<div class="shortcuts-tab">
	<div class="subnav">
		<button class="seg" class:active={activeView === "keys"} onclick={() => (activeView = "keys")}>
			Keys
		</button>
		<button
			class="seg"
			class:active={activeView === "cli"}
			onclick={() => (activeView = "cli")}
		>
			CLI
		</button>
		<button
			class="seg"
			class:active={activeView === "quicklinks"}
			onclick={() => (activeView = "quicklinks")}
		>
			Quicklinks
			{#if quicklinkCount > 0}<span class="count">{quicklinkCount}</span>{/if}
		</button>
		<button
			class="seg"
			class:active={activeView === "aliases"}
			onclick={() => (activeView = "aliases")}
		>
			Aliases
			{#if aliasCount > 0}<span class="count">{aliasCount}</span>{/if}
		</button>
	</div>

	<!-- All three views stay mounted and toggle visibility rather than being
	     created on demand: this WebView pays a large one-time layout cost the
	     first time a subtree is built, so `{#if}` here would make the first
	     switch to each tab visibly janky. -->
	<div class="view">
		<div class:hidden={activeView !== "keys"}>
			<KeysView bind:keybindingsConfig />
		</div>
		<div class:hidden={activeView !== "cli"}>
			<CliView />
		</div>
		<div class:hidden={activeView !== "quicklinks"}>
			<QuicklinksView
				bind:commandsConfig
				{onsaveerror}
				oncount={(n) => (quicklinkCount = n)}
			/>
		</div>
		<div class:hidden={activeView !== "aliases"}>
			<AliasesView {onsaveerror} oncount={(n) => (aliasCount = n)} />
		</div>
	</div>
</div>

<style>
	.shortcuts-tab {
		display: flex;
		flex-direction: column;
		min-height: 0;
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
</style>

<script lang="ts">
import {
	ClipboardList,
	Clock,
	Music,
	Pause,
	Play,
	Settings,
	Sparkles,
	SquareTerminal,
} from "lucide-svelte";
import type { CommandResult, TrackInfo } from "$lib/ipc";
import { mediaControl } from "$lib/ipc";

let {
	result = null,
	executing = false,
	routing = false,
	historyOpen = false,
	settingsOpen = false,
	mediaOpen = false,
	nowPlaying = null,
	multiplePlayers = false,
	ontogglehistory,
	ontogglesettings,
	ontogglemedia,
	ontogglenotes,
	onshowresult,
	onshowplan,
	notesOpen = false,
	hasPlan = false,
}: {
	result: CommandResult | null;
	executing: boolean;
	routing: boolean;
	historyOpen: boolean;
	settingsOpen: boolean;
	mediaOpen: boolean;
	nowPlaying: TrackInfo | null;
	multiplePlayers: boolean;
	ontogglehistory: () => void;
	ontogglesettings: () => void;
	ontogglemedia: () => void;
	ontogglenotes: () => void;
	onshowresult: () => void;
	onshowplan: () => void;
	notesOpen?: boolean;
	hasPlan?: boolean;
} = $props();

let resultVisible = $derived(
	result &&
		(result.output || result.error) &&
		!settingsOpen &&
		!mediaOpen &&
		!historyOpen &&
		!notesOpen,
);

async function togglePlayPause() {
	if (!nowPlaying) return;
	await mediaControl(nowPlaying.bus_name, "play_pause");
}
</script>

<div class="status-bar">
	<div class="status-info">
		{#if routing}
			<span class="routing-text"><span class="traveler"><span class="dot"></span></span> Routing...</span>
		{:else if executing}
			<span class="executing-text"><span class="traveler"><span class="dot"></span></span> Running...</span>
		{:else if result}
			{#if result.routed_by === "ai"}
				<span class="ai-indicator"><Sparkles size={11} strokeWidth={2} /></span>
			{/if}
			<span class="duration">{result.duration_ms}ms</span>
			{#if result.needs_confirmation}
				<span class="status confirm">CONFIRM</span>
			{:else if result.success}
				<span class="status success">OK</span>
			{:else}
				<span class="status error">ERR</span>
			{/if}
		{/if}
	</div>

	{#if nowPlaying && !mediaOpen}
		<!-- svelte-ignore a11y_click_events_have_key_events -->
		<!-- svelte-ignore a11y_no_static_element_interactions -->
		<div class="now-playing" onclick={ontogglemedia}>
			<button
				class="mini-play"
				onmousedown={(e) => e.preventDefault()}
				onclick={(e) => { e.stopPropagation(); togglePlayPause(); }}
				tabindex={-1}
				aria-label={nowPlaying.status === "playing" ? "Pause" : "Play"}
			>
				{#if nowPlaying.status === "playing"}
					<Pause size={10} strokeWidth={2} />
				{:else}
					<Play size={10} strokeWidth={2} />
				{/if}
			</button>
			<span class="now-playing-text">
				{#if multiplePlayers}
					<span class="player-label">{nowPlaying.player_name}:</span>
				{/if}
				{nowPlaying.artist} — {nowPlaying.title}
			</span>
		</div>
	{/if}

	<div class="toolbar">
		{#if result && (result.output || result.error)}
			<button
				class="bar-icon"
				class:active={resultVisible}
				onmousedown={(e) => e.preventDefault()}
				onclick={onshowresult}
				title="Output"
				tabindex={-1}
			>
				<SquareTerminal size={14} strokeWidth={1.5} />
			</button>
		{/if}
		{#if hasPlan}
			<button
				class="bar-icon"
				class:active={!settingsOpen && !mediaOpen && !historyOpen && !(result && (result.output || result.error))}
				onmousedown={(e) => e.preventDefault()}
				onclick={onshowplan}
				title="AI Plan"
				tabindex={-1}
			>
				<Sparkles size={14} strokeWidth={1.5} />
			</button>
		{/if}
		<button
			class="bar-icon"
			class:active={historyOpen}
			onmousedown={(e) => e.preventDefault()}
			onclick={ontogglehistory}
			title="History (Ctrl+1)"
			tabindex={-1}
		>
			<Clock size={14} strokeWidth={1.5} />
		</button>
		<button
			class="bar-icon"
			class:active={notesOpen}
			onmousedown={(e) => e.preventDefault()}
			onclick={ontogglenotes}
			title="Utility (Ctrl+2)"
			tabindex={-1}
		>
			<ClipboardList size={14} strokeWidth={1.5} />
		</button>
		<button
			class="bar-icon"
			class:active={mediaOpen}
			onmousedown={(e) => e.preventDefault()}
			onclick={ontogglemedia}
			title="Media (Ctrl+3)"
			tabindex={-1}
		>
			<Music size={14} strokeWidth={1.5} />
		</button>
		<button
			class="bar-icon"
			class:active={settingsOpen}
			onmousedown={(e) => e.preventDefault()}
			onclick={ontogglesettings}
			title="Settings (Ctrl+4)"
			tabindex={-1}
		>
			<Settings size={14} strokeWidth={1.5} />
		</button>
	</div>
</div>

<style>
	.status-bar {
		display: flex;
		align-items: center;
		justify-content: space-between;
		padding: 4px 20px;
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--fg-muted);
		background: var(--bg);
		gap: 8px;
	}

	.toolbar {
		display: flex;
		gap: 2px;
		flex-shrink: 0;
	}

	.bar-icon {
		display: flex;
		align-items: center;
		justify-content: center;
		background: none;
		border: none;
		color: var(--fg-muted);
		cursor: pointer;
		padding: 4px;
		border-radius: 4px;
		transition: color 100ms ease;
	}

	.bar-icon:hover {
		color: var(--fg);
	}

	.bar-icon.active {
		color: var(--accent);
	}

	.status-info {
		display: flex;
		align-items: center;
		gap: 8px;
		flex-shrink: 0;
	}

	.now-playing {
		display: flex;
		align-items: center;
		gap: 6px;
		min-width: 0;
		cursor: pointer;
		border-radius: 4px;
		padding: 2px 6px;
		transition: background 100ms ease;
	}

	.now-playing:hover {
		background: var(--bg-secondary);
	}

	.mini-play {
		display: flex;
		align-items: center;
		justify-content: center;
		background: none;
		border: none;
		color: var(--fg-muted);
		cursor: pointer;
		padding: 2px;
		border-radius: 3px;
		flex-shrink: 0;
		transition: color 100ms ease;
	}

	.mini-play:hover {
		color: var(--fg);
	}

	.now-playing-text {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		color: var(--fg-muted);
		font-size: 10px;
	}

	.player-label {
		color: var(--accent);
		margin-right: 2px;
	}

	.routing-text,
	.executing-text {
		display: flex;
		align-items: center;
		gap: 4px;
	}

	.routing-text {
		color: var(--accent);
		opacity: 0.6;
	}

	.executing-text {
		color: var(--fg-muted);
	}

	.traveler {
		position: relative;
		display: inline-flex;
		align-items: center;
		width: 14px;
		height: 11px;
		flex-shrink: 0;
	}

	.dot {
		position: absolute;
		top: 50%;
		width: 5px;
		height: 5px;
		border-radius: 50%;
		background: currentColor;
		transform: translate(0, -50%);
		animation: travel 1.2s ease-in-out infinite;
	}

	@keyframes travel {
		0%, 100% { left: 0; }
		50% { left: calc(100% - 5px); }
	}

	.duration {
		color: var(--fg-muted);
	}

	.status {
		font-weight: 600;
		text-transform: uppercase;
	}

	.success {
		color: var(--success);
	}

	.error {
		color: var(--error);
	}

	.confirm {
		color: #ffaa00;
	}

	.ai-indicator {
		color: var(--ai);
		display: flex;
		align-items: center;
	}

</style>

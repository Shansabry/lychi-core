<script lang="ts">
import { onMount } from "svelte";
import type { TrackInfo } from "$lib/ipc";
import { mediaControl, mediaSeek } from "$lib/ipc";

let {
	ondismiss,
	players = [],
}: {
	ondismiss: () => void;
	players: TrackInfo[];
} = $props();
let selectedBusName: string | null = $state(null);
let notRunning = $state(false);
let positionUs = $state(0);
let positionTimer: ReturnType<typeof setInterval> | undefined;

// The currently selected track
let track = $derived.by(() => {
	if (!selectedBusName && players.length > 0) {
		// Auto-select first playing, or first in list
		const playing = players.find((p) => p.status === "playing");
		return playing ?? players[0];
	}
	return players.find((p) => p.bus_name === selectedBusName) ?? null;
});

let progress = $derived(track && track.length_us > 0 ? positionUs / track.length_us : 0);
let positionStr = $derived(formatTime(positionUs));
let durationStr = $derived(track ? formatTime(track.length_us) : "0:00");

function formatTime(us: number): string {
	const secs = Math.floor(us / 1_000_000);
	const m = Math.floor(secs / 60);
	const s = secs % 60;
	return `${m}:${s.toString().padStart(2, "0")}`;
}

function startTimer() {
	clearInterval(positionTimer);
	positionTimer = setInterval(() => {
		if (track?.status === "playing") {
			positionUs = Math.min(positionUs + 1_000_000, track?.length_us ?? positionUs);
		}
	}, 1000);
}

// Sync position when selected track changes
$effect(() => {
	if (track) {
		positionUs = track.position_us;
		clearInterval(positionTimer);
		if (track.status === "playing") startTimer();
	}
});

// Show "not running" if parent passes empty players
$effect(() => {
	notRunning = players.length === 0;
});

onMount(() => {
	return () => {
		clearInterval(positionTimer);
	};
});

async function handleControl(action: string) {
	if (!track) return;
	await mediaControl(track.bus_name, action);
	// Optimistic play/pause toggle — update local timer
	if (action === "play_pause" && track) {
		if (track.status === "playing") {
			clearInterval(positionTimer);
		} else {
			startTimer();
		}
	}
}

async function handleSeek(e: MouseEvent) {
	if (!track || track.length_us <= 0) return;
	const bar = e.currentTarget as HTMLElement;
	const rect = bar.getBoundingClientRect();
	const ratio = Math.max(0, Math.min(1, (e.clientX - rect.left) / rect.width));
	const newPositionUs = Math.floor(ratio * track.length_us);
	positionUs = newPositionUs;
	await mediaSeek(track.bus_name, track.track_id, newPositionUs);
}

function handleKeydown(e: KeyboardEvent) {
	if (e.key === "Escape") {
		e.preventDefault();
		e.stopPropagation();
		ondismiss();
	}
}

function selectPlayer(busName: string) {
	selectedBusName = busName;
}
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="media-panel" onkeydown={handleKeydown}>
	{#if notRunning}
		<div class="not-running">
			<span class="note">♫</span>
			<span>No media players running</span>
		</div>
	{:else if track}
		{#if players.length > 1}
			<div class="player-tabs">
				{#each players as player}
					<button
						class="player-tab"
						class:active={player.bus_name === track.bus_name}
						onclick={() => selectPlayer(player.bus_name)}
					>
						{player.player_name}
						{#if player.status === "playing"}
							<span class="playing-dot"></span>
						{/if}
					</button>
				{/each}
			</div>
		{/if}

		<div class="now-playing">
			{#if track.art_url}
				<img class="album-art" src={track.art_url} alt="Album art" />
			{:else}
				<div class="album-art-placeholder">♫</div>
			{/if}

			<div class="track-info">
				<div class="title">{track.title || "Unknown"}</div>
				<div class="artist">{track.artist || "Unknown artist"}</div>
				{#if track.album}
					<div class="album">{track.album}</div>
				{/if}
			</div>
		</div>

		<!-- svelte-ignore a11y_click_events_have_key_events -->
		<div class="progress-section">
			<span class="time">{positionStr}</span>
			<div class="progress-bar" role="slider" tabindex={0} aria-valuenow={positionUs} aria-valuemin={0} aria-valuemax={track.length_us} onclick={handleSeek}>
				<div class="progress-fill" style="width: {progress * 100}%"></div>
			</div>
			<span class="time">{durationStr}</span>
		</div>

		<div class="controls">
			<button class="ctrl-btn" onclick={() => handleControl("prev")} aria-label="Previous">⏮</button>
			<button class="ctrl-btn play" onclick={() => handleControl("play_pause")} aria-label="Play/Pause">
				{track.status === "playing" ? "⏸" : "▶"}
			</button>
			<button class="ctrl-btn" onclick={() => handleControl("next")} aria-label="Next">⏭</button>
		</div>
	{:else}
		<div class="loading">Loading...</div>
	{/if}
</div>

<style>
	.media-panel {
		padding: 16px 20px;
		font-family: var(--font-mono);
		color: var(--fg);
	}

	.player-tabs {
		display: flex;
		gap: 2px;
		margin-bottom: 14px;
		padding-bottom: 8px;
		border-bottom: 1px solid var(--bg-secondary);
	}

	.player-tab {
		display: flex;
		align-items: center;
		gap: 5px;
		background: none;
		border: none;
		color: var(--fg-muted);
		font-family: var(--font-mono);
		font-size: 11px;
		cursor: pointer;
		padding: 4px 10px;
		border-radius: 4px;
		transition: color 100ms ease, background 100ms ease;
	}

	.player-tab:hover {
		color: var(--fg);
		background: var(--bg-secondary);
	}

	.player-tab.active {
		color: var(--accent);
		background: var(--bg-secondary);
	}

	.playing-dot {
		width: 5px;
		height: 5px;
		border-radius: 50%;
		background: var(--accent);
		flex-shrink: 0;
	}

	.not-running {
		display: flex;
		align-items: center;
		gap: 10px;
		padding: 12px 0;
		color: var(--fg-muted);
		font-size: 13px;
	}

	.note {
		font-size: 18px;
		opacity: 0.5;
	}

	.now-playing {
		display: flex;
		gap: 14px;
		align-items: flex-start;
		margin-bottom: 14px;
	}

	.album-art {
		width: 64px;
		height: 64px;
		border-radius: 6px;
		object-fit: cover;
		flex-shrink: 0;
		background: var(--bg-secondary);
	}

	.album-art-placeholder {
		width: 64px;
		height: 64px;
		border-radius: 6px;
		background: var(--bg-secondary);
		display: flex;
		align-items: center;
		justify-content: center;
		font-size: 24px;
		color: var(--fg-muted);
		flex-shrink: 0;
	}

	.track-info {
		display: flex;
		flex-direction: column;
		gap: 3px;
		min-width: 0;
		flex: 1;
	}

	.title {
		font-size: 14px;
		font-weight: 600;
		color: var(--fg);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	.artist {
		font-size: 12px;
		color: var(--fg-muted);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	.album {
		font-size: 11px;
		color: var(--fg-muted);
		opacity: 0.7;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	.progress-section {
		display: flex;
		align-items: center;
		gap: 8px;
		margin-bottom: 14px;
	}

	.time {
		font-size: 11px;
		color: var(--fg-muted);
		font-family: var(--font-mono);
		flex-shrink: 0;
		min-width: 32px;
	}

	.progress-bar {
		flex: 1;
		height: 3px;
		background: var(--bg-secondary);
		border-radius: 2px;
		overflow: hidden;
		cursor: pointer;
		/* Enlarge click target without affecting layout */
		margin: -6px 0;
		padding: 6px 0;
		box-sizing: content-box;
		background-clip: content-box;
	}

	.progress-fill {
		height: 100%;
		background: var(--fg);
		border-radius: 2px;
		transition: width 1s linear;
	}

	.controls {
		display: flex;
		align-items: center;
		justify-content: center;
		gap: 20px;
	}

	.ctrl-btn {
		background: none;
		border: none;
		color: var(--fg-muted);
		font-size: 18px;
		cursor: pointer;
		padding: 4px;
		border-radius: 4px;
		transition: color 80ms ease;
		line-height: 1;
	}

	.ctrl-btn:hover {
		color: var(--fg);
	}

	.ctrl-btn.play {
		font-size: 22px;
		color: var(--fg);
	}

	.loading {
		padding: 12px 0;
		color: var(--fg-muted);
		font-size: 13px;
	}
</style>

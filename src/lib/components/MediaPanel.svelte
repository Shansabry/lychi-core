<script lang="ts">
import {
	Pause,
	Play,
	Repeat,
	Repeat1,
	RotateCcw,
	RotateCw,
	Shuffle,
	SkipBack,
	SkipForward,
	Volume1,
	Volume2,
	VolumeX,
} from "lucide-svelte";
import { onMount } from "svelte";
import MediaViz from "$lib/components/MediaViz.svelte";
import type { TrackInfo } from "$lib/ipc";
import {
	mediaControl,
	mediaSeek,
	mediaSeekRelative,
	mediaSetLoop,
	mediaSetShuffle,
	mediaSetVolume,
} from "$lib/ipc";

let {
	ondismiss,
	players = [],
	visible = true,
}: {
	ondismiss: () => void;
	players: TrackInfo[];
	/** Whether the panel is currently shown. The panel stays mounted and is
	 * toggled via a CSS class, so we gate the 1s progress ticker (and its CSS
	 * width transition) on this — a hidden-but-animating progress bar is a
	 * continuous WebView repaint cost for no visible benefit. */
	visible?: boolean;
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

// Art URLs that failed to render (a corrupt data: URI, say) → fall back to the
// note placeholder rather than a broken-image icon. Keyed by the exact src.
let brokenArt = $state(new Set<string>());
function onArtError(src: string | null) {
	if (!src) return;
	const next = new Set(brokenArt);
	next.add(src);
	brokenArt = next;
}
let artOk = $derived(!!track?.art_url && !brokenArt.has(track.art_url));

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

// Sync position when selected track changes, and drive the ticker only while
// the panel is visible. When hidden, the timer is cleared so no per-second
// repaint happens off-screen; on re-show it resyncs from the latest position.
$effect(() => {
	if (track && visible) {
		positionUs = track.position_us;
		clearInterval(positionTimer);
		if (track.status === "playing") startTimer();
	} else if (!visible) {
		clearInterval(positionTimer);
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

// ── Extended controls (shuffle / repeat / volume / time-skip) ──────────────
// Every control is capability-gated: MPRIS makes shuffle/loop/volume/seek
// optional, so we only render a control the active player actually advertises
// (via the flags on TrackInfo) rather than showing a dead button.

// Long-form = a track long enough that ±10s navigation is useful (podcasts,
// videos, long mixes). For music, track-skip is the right control; for long-form
// the row swaps in time-skip flanking play (the industry convention). 20 min is
// a conservative threshold that won't trip on ordinary songs.
const LONG_FORM_THRESHOLD_US = 20 * 60 * 1_000_000;
let isLongForm = $derived(!!track && track.can_seek && track.length_us > LONG_FORM_THRESHOLD_US);

async function toggleShuffle() {
	if (!track || track.shuffle == null) return;
	await mediaSetShuffle(track.bus_name, !track.shuffle);
}

// Cycle repeat: None → Playlist (repeat-all) → Track (repeat-one) → None.
async function cycleLoop() {
	if (!track || track.loop_status == null) return;
	const next =
		track.loop_status === "None" ? "Playlist" : track.loop_status === "Playlist" ? "Track" : "None";
	await mediaSetLoop(track.bus_name, next);
}

async function skip(offsetSecs: number) {
	if (!track) return;
	// Optimistic local position update so the scrub bar responds immediately.
	positionUs = Math.max(0, Math.min(positionUs + offsetSecs * 1_000_000, track.length_us));
	await mediaSeekRelative(track.bus_name, offsetSecs * 1_000_000);
}

// Volume: hover-reveal (kept off the always-visible row per compact-panel
// convention). Local echo so the slider tracks the drag before the poll catches up.
let volumeOpen = $state(false);
let localVolume = $state<number | null>(null);
// Volume captured before muting, so clicking the speaker restores it. Cleared
// once the user drags the slider (a manual change supersedes the mute memory).
let preMuteVolume = $state<number | null>(null);
let displayVolume = $derived(localVolume ?? track?.volume ?? 0);
async function setVolume(v: number) {
	if (!track || track.volume == null) return;
	preMuteVolume = null; // an explicit drag ends any mute-restore memory
	localVolume = v;
	await mediaSetVolume(track.bus_name, v);
}
// Clicking the speaker mutes (→ 0, remembering the level) or unmutes (→ the
// remembered level, or 0.5 as a sane default if we muted before we knew it).
async function toggleMute() {
	if (!track || track.volume == null) return;
	if (displayVolume > 0) {
		preMuteVolume = displayVolume;
		localVolume = 0;
		await mediaSetVolume(track.bus_name, 0);
	} else {
		const restore = preMuteVolume ?? 0.5;
		preMuteVolume = null;
		localVolume = restore;
		await mediaSetVolume(track.bus_name, restore);
	}
}
// Drop the local echo once the real value catches up to it (or the track changes).
$effect(() => {
	if (track && localVolume != null && Math.abs((track.volume ?? 0) - localVolume) < 0.02) {
		localVolume = null;
	}
});

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
			{#if artOk}
				<img
					class="album-art"
					src={track.art_url}
					alt="Album art"
					onerror={() => onArtError(track.art_url)}
				/>
			{:else}
				<div class="album-art-placeholder">♫</div>
			{/if}

			<div class="track-info">
				<div class="title-row">
					<!-- Now-playing waveform — the shared MediaViz (same one the status
					     bar's now-playing pill uses), so they never drift apart. -->
					<MediaViz playing={track.status === 'playing'} height={12} />
					<div class="title">{track.title || "Unknown"}</div>
				</div>
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
				<div class="progress-fill" class:animate={visible} style="width: {progress * 100}%"></div>
			</div>
			<span class="time">{durationStr}</span>
		</div>

		<div class="controls">
			<!-- Shuffle (left, canonical). Only shown if the player advertises it.
			     Active = accent tint + a dot beneath (non-color cue). -->
			{#if track.shuffle != null}
				<button
					class="ctrl-btn toggle"
					class:on={track.shuffle}
					onclick={toggleShuffle}
					aria-pressed={track.shuffle}
					aria-label="Shuffle"
					title="Shuffle"
				><Shuffle size={15} strokeWidth={1.75} /></button>
			{/if}

			<!-- Long-form: time-skip flanks play. Music: track-skip. -->
			{#if isLongForm}
				<button class="ctrl-btn skip" onclick={() => skip(-10)} aria-label="Back 10 seconds" title="Back 10s">
					<span class="skip-glyph"><RotateCcw size={17} strokeWidth={1.75} /><span class="skip-n">10</span></span>
				</button>
			{:else if track.can_go_previous}
				<button class="ctrl-btn" onclick={() => handleControl("prev")} aria-label="Previous" title="Previous"><SkipBack size={16} strokeWidth={1.75} /></button>
			{/if}

			<button class="ctrl-btn play" onclick={() => handleControl("play_pause")} aria-label="Play/Pause" title="Play/Pause">
				{#if track.status === "playing"}<Pause size={19} strokeWidth={1.75} fill="currentColor" />{:else}<Play size={19} strokeWidth={1.75} fill="currentColor" />{/if}
			</button>

			{#if isLongForm}
				<button class="ctrl-btn skip" onclick={() => skip(30)} aria-label="Forward 30 seconds" title="Forward 30s">
					<span class="skip-glyph"><RotateCw size={17} strokeWidth={1.75} /><span class="skip-n">30</span></span>
				</button>
			{:else if track.can_go_next}
				<button class="ctrl-btn" onclick={() => handleControl("next")} aria-label="Next" title="Next"><SkipForward size={16} strokeWidth={1.75} /></button>
			{/if}

			<!-- Repeat (right, canonical). Tri-state cycle; label carries the state.
			     Repeat-one uses Lucide's dedicated Repeat1 glyph (the "1" is a
			     non-color cue), like Spotify/YTM. -->
			{#if track.loop_status != null}
				<button
					class="ctrl-btn toggle repeat"
					class:on={track.loop_status !== 'None'}
					onclick={cycleLoop}
					aria-label={track.loop_status === 'None' ? 'Repeat off' : track.loop_status === 'Track' ? 'Repeat one' : 'Repeat all'}
					title={track.loop_status === 'None' ? 'Repeat: off' : track.loop_status === 'Track' ? 'Repeat: one' : 'Repeat: all'}
				>
					{#if track.loop_status === 'Track'}<Repeat1 size={15} strokeWidth={1.75} />{:else}<Repeat size={15} strokeWidth={1.75} />{/if}
				</button>
			{/if}

			<!-- Volume: hover-reveal on a speaker icon (compact-panel convention;
			     omitted from the always-visible row). Only if the player exposes Volume. -->
			{#if track.volume != null}
				<!-- svelte-ignore a11y_no_static_element_interactions -->
				<div
					class="volume"
					class:open={volumeOpen}
					onmouseenter={() => { volumeOpen = true; }}
					onmouseleave={() => { volumeOpen = false; }}
				>
					<button
						class="ctrl-btn"
						class:on={displayVolume === 0}
						aria-label={displayVolume === 0 ? 'Unmute' : 'Mute'}
						aria-pressed={displayVolume === 0}
						title={displayVolume === 0 ? 'Unmute' : 'Mute'}
						onclick={toggleMute}
					>{#if displayVolume === 0}<VolumeX size={16} strokeWidth={1.75} />{:else if displayVolume < 0.5}<Volume1 size={16} strokeWidth={1.75} />{:else}<Volume2 size={16} strokeWidth={1.75} />{/if}</button>
					<input
						class="volume-slider"
						type="range"
						min="0"
						max="1"
						step="0.02"
						value={displayVolume}
						aria-label="Volume"
						aria-valuetext={`${Math.round(displayVolume * 100)}%`}
						oninput={(e) => setVolume(Number((e.currentTarget as HTMLInputElement).value))}
					/>
				</div>
			{/if}
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
		cursor: pointer;
		/* Enlarge click target without affecting layout. No `overflow: hidden` —
		   it would clip the fill's soft accent glow; the fill has its own
		   border-radius, and background-clip keeps the track itself tidy. */
		margin: -6px 0;
		padding: 6px 0;
		box-sizing: content-box;
		background-clip: content-box;
	}

	.progress-fill {
		height: 100%;
		border-radius: 2px;
		/* Gradient played portion: a dim accent at the start brightening to full
		   accent at the leading edge (the playhead), so it reads as filling toward
		   "now". color-mix keeps both stops tied to the theme accent. No glow. */
		background: linear-gradient(
			90deg,
			color-mix(in srgb, var(--accent) 45%, transparent) 0%,
			var(--accent) 100%
		);
	}

	/* Only animate the fill while the panel is visible. When hidden the width is
	 * frozen; gating the transition means re-showing snaps to the current
	 * position instead of sliding from a stale one, and avoids off-screen paint. */
	.progress-fill.animate {
		transition: width 1s linear;
	}

	.controls {
		display: flex;
		align-items: center;
		justify-content: center;
		gap: 18px;
		/* Anchor for the volume control, which is positioned at the right edge so
		   its hover-expanding slider never shifts the centered transport row. */
		position: relative;
	}

	/* Fixed-size flex boxes that center their glyph, so prev/play/next line up on
	   a shared centerline regardless of each media glyph's own baseline/metrics
	   (unicode ⏮ ▶ ⏭ sit differently on the text baseline). */
	.ctrl-btn {
		display: flex;
		align-items: center;
		justify-content: center;
		width: 34px;
		height: 34px;
		background: none;
		border: none;
		color: var(--fg-muted);
		font-size: 18px;
		line-height: 1;
		cursor: pointer;
		border-radius: 50%;
		transition: color 80ms ease, background 80ms ease;
	}

	.ctrl-btn:hover {
		color: var(--fg);
		background: var(--bg-secondary);
	}

	.ctrl-btn.play {
		font-size: 22px;
		color: var(--accent);
	}

	.ctrl-btn.play:hover {
		color: var(--accent);
		background: var(--bg-secondary);
		filter: brightness(1.15);
	}

	/* Toggle buttons (shuffle/repeat): smaller glyph; active = accent tint PLUS a
	   dot beneath — a non-color cue so on/off is readable without relying on the
	   accent hue (the Spotify/YTM pattern; accent-alone fails use-of-color). */
	.ctrl-btn.toggle {
		font-size: 15px;
		position: relative;
	}
	.ctrl-btn.toggle.on {
		color: var(--accent);
	}
	.ctrl-btn.toggle.on::after {
		content: "";
		position: absolute;
		bottom: 2px;
		left: 50%;
		transform: translateX(-50%);
		width: 3px;
		height: 3px;
		border-radius: 50%;
		background: var(--accent);
	}
	/* Time-skip glyph: a circular arrow (RotateCcw/Cw) with the second-count
	   overlaid in its centre, so it reads as "move within this item" — never a
	   bare skip-forward (that means next track). */
	.skip-glyph {
		position: relative;
		display: inline-flex;
		align-items: center;
		justify-content: center;
	}
	.skip-n {
		position: absolute;
		top: 50%;
		left: 50%;
		transform: translate(-50%, -42%);
		font-size: 7px;
		font-weight: 700;
		font-family: var(--font-mono);
		pointer-events: none;
	}

	/* Volume: a speaker button that reveals a horizontal slider on hover/focus.
	   Kept off the always-visible row (compact-panel convention). */
	/* Anchored to the right edge, OUT of the centered flow, so the transport
	   buttons stay put when the slider expands. The slider grows leftward from the
	   speaker (flex-direction: row-reverse) so it never pushes past the edge. */
	.volume {
		position: absolute;
		right: 0;
		top: 50%;
		transform: translateY(-50%);
		display: flex;
		flex-direction: row-reverse;
		align-items: center;
	}
	.volume-slider {
		width: 0;
		opacity: 0;
		margin-right: 0;
		accent-color: var(--accent);
		cursor: pointer;
		transition: width 140ms ease, opacity 140ms ease, margin-right 140ms ease;
	}
	.volume.open .volume-slider,
	.volume:hover .volume-slider,
	.volume:focus-within .volume-slider {
		width: 72px;
		opacity: 1;
		margin-right: 6px;
	}

	/* Now-playing equalizer bars. */
	.title-row {
		display: flex;
		align-items: center;
		gap: 7px;
	}

	.loading {
		padding: 12px 0;
		color: var(--fg-muted);
		font-size: 13px;
	}
</style>

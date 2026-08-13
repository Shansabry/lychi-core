<script lang="ts">
import {
	ClipboardList,
	Clock,
	Lightbulb,
	LoaderCircle,
	MessageSquare,
	Music,
	Settings,
	Sparkles,
	SquareTerminal,
} from "lucide-svelte";
import MediaViz from "$lib/components/MediaViz.svelte";
import type { CommandResult, TrackInfo } from "$lib/ipc";
import { mediaControl } from "$lib/ipc";
import { getComboString } from "$lib/keybindings";

let {
	result = null,
	executing = false,
	historyOpen = false,
	settingsOpen = false,
	mediaOpen = false,
	chatHistoryOpen = false,
	nowPlaying = null,
	multiplePlayers = false,
	ontogglehistory,
	ontogglesettings,
	ontogglemedia,
	ontogglenotes,
	ontogglechathistory = () => {},
	onshowresult,
	onshowai = () => {},
	notesOpen = false,
	aiParked = false,
	contextStale = false,
	contextStaleHint = "",
	contextRefreshing = false,
	actionsAvailable = false,
	onactionpanel = () => {},
	aiLoading = false,
}: {
	result: CommandResult | null;
	executing: boolean;
	historyOpen: boolean;
	settingsOpen: boolean;
	mediaOpen: boolean;
	chatHistoryOpen?: boolean;
	nowPlaying: TrackInfo | null;
	multiplePlayers: boolean;
	ontogglehistory: () => void;
	ontogglesettings: () => void;
	ontogglemedia: () => void;
	ontogglenotes: () => void;
	ontogglechathistory?: () => void;
	onshowresult: () => void;
	onshowai?: () => void;
	notesOpen?: boolean;
	aiParked?: boolean;
	contextStale?: boolean;
	contextStaleHint?: string;
	contextRefreshing?: boolean;
	actionsAvailable?: boolean;
	onactionpanel?: () => void;
	aiLoading?: boolean;
} = $props();

let resultVisible = $derived(
	result &&
		(result.output || result.error) &&
		!settingsOpen &&
		!mediaOpen &&
		!historyOpen &&
		!notesOpen,
);

// The left status-info slot has transient content to show (running / AI loading
// / a result / a context hint). When it does, the now-playing media pill yields
// its space so that content is unobstructed; once the slot clears, the pill
// slides back in. This keeps the busy state legible without crowding.
let statusBusy = $derived(
	aiLoading || executing || Boolean(result) || contextRefreshing || contextStale,
);

async function togglePlayPause() {
	if (!nowPlaying) return;
	await mediaControl(nowPlaying.bus_name, "play_pause");
}
</script>

<div class="status-bar">
	<div class="status-info">
		{#if aiLoading}
			<span class="ai-loading"><Sparkles size={11} strokeWidth={2} /> Loading AI model…</span>
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
		{:else if contextRefreshing}
			<!-- A background re-gather is actually in flight → honest spinner. -->
			<span class="stale-hint refreshing">
				<LoaderCircle size={11} strokeWidth={2} />
				<span class="stale-text">updating context…</span>
			</span>
		{:else if contextStale}
			<!-- Idle & stale: a dim bulb (NOT a spinner — nothing is loading; state
			     refreshes on the next summon/command). Small label + hover detail,
			     so it never crowds the suggestion list. -->
			<span class="stale-hint" title={contextStaleHint}>
				<Lightbulb size={11} strokeWidth={2} />
				<span class="stale-text">context outdated</span>
			</span>
		{/if}
	</div>

	{#if nowPlaying && !mediaOpen}
		<!-- svelte-ignore a11y_click_events_have_key_events -->
		<!-- svelte-ignore a11y_no_static_element_interactions -->
		<div
			class="now-playing"
			class:yielded={statusBusy}
			onclick={ontogglemedia}
		>
			<!-- Left slot: the animated equalizer IS the playing indicator + the
			     pause control. While playing it shows the bouncing waveform; on
			     hover / when paused it's the play/pause button. Clicking it toggles
			     playback (stopPropagation so it doesn't open the panel). MPRIS gives
			     no real audio stream, so the waveform is a synthetic bounce. -->
			<button
				class="viz-btn"
				onmousedown={(e) => e.preventDefault()}
				onclick={(e) => { e.stopPropagation(); togglePlayPause(); }}
				tabindex={-1}
				aria-label={nowPlaying.status === "playing" ? "Pause" : "Play"}
			>
				<!-- The waveform: animated while playing, frozen + muted when paused.
				     Shared component (see MediaViz) so the panel and the pill match. -->
				<MediaViz playing={nowPlaying.status === "playing"} />
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
		{#if actionsAvailable}
			<!-- Discoverable ⌘K affordance: teaches the shortcut and gives AT users a
			     focusable trigger for the actions menu (aria-haspopup="menu"). -->
			<button
				class="actions-hint"
				onmousedown={(e) => e.preventDefault()}
				onclick={onactionpanel}
				title="Show actions for the selected result"
				aria-haspopup="menu"
				tabindex={-1}
			>
				<span>Actions</span>
				<kbd>{getComboString("action_panel")}</kbd>
			</button>
		{/if}
		{#if aiParked}
			<!-- An AI answer/chat exists but is parked off-stage (behind a panel or
			     hidden via Esc). This icon recalls it — nothing is lost. Only shown
			     while the answer is off-screen. -->
			<button
				class="bar-icon ai-parked"
				onmousedown={(e) => e.preventDefault()}
				onclick={onshowai}
				title="Show AI answer"
				tabindex={-1}
			>
				<Sparkles size={14} strokeWidth={1.5} />
			</button>
		{/if}
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
		{#if (result && (result.output || result.error)) || aiParked}
			<span class="toolbar-sep"></span>
		{/if}
		<button
			class="bar-icon"
			class:active={chatHistoryOpen}
			onmousedown={(e) => e.preventDefault()}
			onclick={ontogglechathistory}
			title="AI chat history (type: chat)"
			tabindex={-1}
		>
			<MessageSquare size={14} strokeWidth={1.5} />
		</button>
		<button
			class="bar-icon"
			class:active={historyOpen}
			onmousedown={(e) => e.preventDefault()}
			onclick={ontogglehistory}
			title="History ({getComboString('toggle_history')})"
			tabindex={-1}
		>
			<Clock size={14} strokeWidth={1.5} />
		</button>
		<button
			class="bar-icon"
			class:active={notesOpen}
			onmousedown={(e) => e.preventDefault()}
			onclick={ontogglenotes}
			title="Utility ({getComboString('toggle_notes')})"
			tabindex={-1}
		>
			<ClipboardList size={14} strokeWidth={1.5} />
		</button>
		<button
			class="bar-icon"
			class:active={mediaOpen}
			onmousedown={(e) => e.preventDefault()}
			onclick={ontogglemedia}
			title="Media ({getComboString('toggle_media')})"
			tabindex={-1}
		>
			<Music size={14} strokeWidth={1.5} />
		</button>
		<button
			class="bar-icon"
			class:active={settingsOpen}
			onmousedown={(e) => e.preventDefault()}
			onclick={ontogglesettings}
			title="Settings ({getComboString('toggle_settings')})"
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
		/* Left group ([media][status-info]) packs against the left edge; the
		   toolbar is pushed to the far right via margin-left:auto below. */
		justify-content: flex-start;
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
		align-items: center;
		flex-shrink: 0;
		/* Pin to the far right regardless of how much the left group fills. */
		margin-left: auto;
	}

	.toolbar-sep {
		width: 1px;
		height: 10px;
		background: var(--border);
		margin: 0 4px;
		flex-shrink: 0;
	}

	.actions-hint {
		display: flex;
		align-items: center;
		gap: 5px;
		background: none;
		border: none;
		color: var(--fg-muted);
		font-family: var(--font-mono);
		font-size: 11px;
		padding: 2px 6px;
		border-radius: 4px;
		cursor: pointer;
		margin-right: 2px;
		transition: color 100ms ease, background 100ms ease;
	}

	.actions-hint:hover {
		color: var(--fg);
		background: var(--bg-secondary);
	}

	.actions-hint kbd {
		font-family: var(--font-mono);
		font-size: 10px;
		color: var(--fg-muted);
		background: var(--bg-secondary);
		border: 1px solid var(--border);
		border-radius: 3px;
		padding: 0 4px;
		line-height: 1.5;
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

	/* Parked AI answer: tinted with the AI hue + a soft pulse so it reads as
	   "answer waiting" rather than an inert toggle. */
	.bar-icon.ai-parked {
		color: var(--accent);
		animation: ai-park-pulse 2s ease-in-out infinite;
	}

	.bar-icon.ai-parked:hover {
		opacity: 1;
		animation: none;
	}

	@keyframes ai-park-pulse {
		0%, 100% { opacity: 0.7; }
		50% { opacity: 1; }
	}

	.status-info {
		display: flex;
		align-items: center;
		gap: 8px;
		flex-shrink: 0;
	}

	.now-playing {
		/* Pinned to the far bottom-left: render before .status-info (order:-1) and
		   sit hard against the bar's left padding, with status-info following it to
		   the right. The auto right-margin that pushes the toolbar over lives on
		   .status-info (see below), so this stays flush left. */
		order: -1;
		display: flex;
		align-items: center;
		gap: 6px;
		min-width: 0;
		cursor: pointer;
		border-radius: 4px;
		padding: 2px 6px;
		/* Transition opacity + width so it collapses/returns smoothly when the
		   status-info slot needs the space. will-change keeps it on the GPU layer
		   (hot path — toggles often). */
		transition:
			background 100ms ease,
			opacity 160ms ease,
			max-width 200ms ease,
			transform 200ms ease;
		will-change: opacity, max-width, transform;
		max-width: 300px;
		overflow: hidden;
	}

	.now-playing:hover {
		background: var(--bg-secondary);
	}

	/* Yielded: the status-info slot has something to show → the media pill fades
	   out, collapses its width, and nudges left, so the busy state is unobstructed.
	   It's kept in the DOM (not removed) so it returns instantly and cheaply. */
	.now-playing.yielded {
		opacity: 0;
		max-width: 0;
		transform: translateX(-6px);
		padding-left: 0;
		padding-right: 0;
		pointer-events: none;
	}

	/* Equalizer visualizer: 4 accent pill-bars that grow from the CENTER (not the
	   floor) — transform-origin: center + scaleY. The organic/random look (vs. a
	   marching left-to-right wave) comes from THREE distinct keyframe curves whose
	   crests land at different heights + different times, plus a distinct duration
	   per bar so they drift out of phase forever. Springy cubic-bezier (never
	   linear). Floors never hit 0 so bars stay alive. Paused = frozen mid + dim via
	   animation-play-state. GPU-friendly: only transform animates. */
	/* The now-playing waveform lives in the shared MediaViz component. */

	/* Left slot: holds either the waveform (playing) or the play glyph (paused).
	   Fixed width so the pill doesn't jump when swapping between them. */
	.viz-btn {
		display: flex;
		align-items: center;
		justify-content: center;
		width: 18px;
		height: 14px;
		background: none;
		border: none;
		color: var(--accent);
		cursor: pointer;
		padding: 0;
		border-radius: 3px;
		flex-shrink: 0;
		transition: opacity 100ms ease, color 100ms ease;
	}

	/* Hover hint that the waveform is a pause control: dim it. */
	.viz-btn:hover {
		opacity: 0.6;
	}

	.now-playing-text {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		color: var(--fg-muted);
		font-size: 10px;
		max-width: 240px;
		opacity: 0.8;
	}

	.player-label {
		color: var(--accent);
		margin-right: 2px;
	}

	.executing-text,
	.ai-loading {
		display: flex;
		align-items: center;
		gap: 4px;
	}

	.ai-loading {
		color: var(--accent);
		animation: pulse 1.4s ease-in-out infinite;
	}

	@keyframes pulse {
		0%, 100% { opacity: 1; }
		50% { opacity: 0.5; }
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
		color: var(--warning);
	}

	.ai-indicator {
		color: var(--accent);
		display: flex;
		align-items: center;
	}

	/* Context-staleness hint — dim and ambient, cursor:help to invite the hover. */
	.stale-hint {
		display: flex;
		align-items: center;
		gap: 4px;
		color: var(--fg-muted);
		opacity: 0.5;
		cursor: help;
		transition: opacity 120ms ease;
	}

	.stale-hint:hover {
		opacity: 0.85;
	}

	.stale-text {
		font-size: 10px;
		white-space: nowrap;
	}

	/* Active refresh: a real spinner (nothing help-able, so default cursor). */
	.stale-hint.refreshing {
		cursor: default;
		opacity: 0.6;
	}

	.stale-hint.refreshing :global(svg) {
		animation: spin 700ms linear infinite;
	}

	@keyframes spin {
		from { transform: rotate(0deg); }
		to { transform: rotate(360deg); }
	}

</style>

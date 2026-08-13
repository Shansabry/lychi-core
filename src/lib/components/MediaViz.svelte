<script lang="ts">
// The now-playing audio waveform — the ONE animated-equalizer indicator, shared
// by the status-bar now-playing pill and the media panel so they can never
// drift apart. Decorative: MPRIS exposes no audio stream, so the bounce is
// synthetic. Four bars, each with a distinct accent-anchored color and an
// organic (non-syncing) animation; animates while `playing`, freezes dimmed +
// desaturated when paused so it clearly reads "stopped".
let {
	playing = false,
	/** Bar height in px. Status bar uses 10; a larger surface can go bigger. */
	height = 10,
}: {
	playing?: boolean;
	height?: number;
} = $props();
</script>

<span class="viz" class:playing aria-hidden="true" style="height: {height}px;">
	<span></span><span></span><span></span><span></span>
</span>

<style>
	.viz {
		display: inline-flex;
		/* Center-anchored: bars scale from the middle (grow both ways). */
		align-items: center;
		gap: 2px;
		flex-shrink: 0;
		/* Paused (default): frozen mid-height, dimmed + desaturated so it clearly
		   reads "stopped" — a quiet, muted version of the playing waveform. */
		opacity: 0.4;
		filter: saturate(0.6);
		transition: opacity 150ms ease, filter 150ms ease;
	}

	.viz span {
		/* Whole-pixel width: a fractional (1.5px) width rounds to 1px on some bars
		   and 2px on others, making them look uneven. 2px renders crisp + uniform. */
		width: 2px;
		height: 100%;
		border-radius: 2px;
		transform: scaleY(0.35);
		transform-origin: center;
		will-change: transform;
		animation-iteration-count: infinite;
		/* Smooth, gentle easing (not springy) — each keyframe segment eases in and
		   out so there are no abrupt direction snaps. */
		animation-timing-function: ease-in-out;
		animation-play-state: paused;
	}

	/* Playing: full color, and the bars animate. */
	.viz.playing {
		opacity: 1;
		filter: none;
	}

	.viz.playing span {
		animation-play-state: running;
	}

	/* Per-bar: a distinct color + keyframe curve + duration + non-round negative
	   delay (starts mid-cycle, never syncs into a wave). Colors span an accent-
	   anchored gradient (accent → a warmer/cooler sibling) so it's multicolor but
	   still on-theme; --accent stays the dominant hue. */
	.viz span:nth-child(1) { background: var(--accent); animation-name: eq-a; animation-duration: 1.9s; animation-delay: -0.8s; }
	.viz span:nth-child(2) { background: color-mix(in srgb, var(--accent) 60%, #ff5ea8); animation-name: eq-c; animation-duration: 2.4s; animation-delay: -1.7s; }
	.viz span:nth-child(3) { background: color-mix(in srgb, var(--accent) 55%, #6ea8ff); animation-name: eq-b; animation-duration: 2.1s; animation-delay: -2.3s; }
	.viz span:nth-child(4) { background: color-mix(in srgb, var(--accent) 65%, #ffcf5e); animation-name: eq-a; animation-duration: 2.6s; animation-delay: -0.4s; }

	/* Keyframes LOOP SEAMLESSLY (0% == 100%) so there's no jump at the wrap, with
	   evenly-spaced stops and gentle swings so the motion flows continuously. */
	@keyframes eq-a {
		0%   { transform: scaleY(0.40); }
		25%  { transform: scaleY(1.00); }
		50%  { transform: scaleY(0.55); }
		75%  { transform: scaleY(0.80); }
		100% { transform: scaleY(0.40); }
	}
	@keyframes eq-b {
		0%   { transform: scaleY(0.85); }
		25%  { transform: scaleY(0.45); }
		50%  { transform: scaleY(1.00); }
		75%  { transform: scaleY(0.55); }
		100% { transform: scaleY(0.85); }
	}
	@keyframes eq-c {
		0%   { transform: scaleY(0.55); }
		25%  { transform: scaleY(0.90); }
		50%  { transform: scaleY(0.40); }
		75%  { transform: scaleY(0.95); }
		100% { transform: scaleY(0.55); }
	}

	/* Respect reduced-motion: no bounce, steady mid height. */
	@media (prefers-reduced-motion: reduce) {
		.viz span {
			animation: none;
			transform: scaleY(0.6);
		}
	}
</style>

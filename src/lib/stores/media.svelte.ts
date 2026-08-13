/**
 * MediaState — MPRIS player list, the derived "now playing", and the status poll.
 *
 * Extracted from +page.svelte (Phase 4). Fully self-contained: it owns the player
 * list, derives `nowPlaying`/`multiplePlayers`, and runs its own poll loop. The
 * page calls `media.start()` once (in onMount) and disposes the returned function
 * on teardown — the ticker lives here rather than as a page `$effect`.
 *
 * `pollTimer` is a plain non-reactive field (a timer handle, nothing renders it).
 */

import { mediaGetStatus, type TrackInfo } from "$lib/ipc";

class MediaState {
	/** Live players (title present OR not stopped). Replaced wholesale. */
	players = $state.raw<TrackInfo[]>([]);

	/** Non-reactive poll-timer handle. */
	pollTimer: ReturnType<typeof setTimeout> | undefined;

	/** The track to show: first playing, else first in the list, else null. */
	get nowPlaying(): TrackInfo | null {
		return this.players.find((p) => p.status === "playing") ?? this.players[0] ?? null;
	}

	get multiplePlayers(): boolean {
		return this.players.length > 1;
	}

	/** Upsert a single player from a `lychi://media-track` event (replace the
	 *  matching bus_name, else append). Matches the original listener exactly;
	 *  the poll loop is what prunes stopped/empty players. */
	applyTrack = (track: TrackInfo): void => {
		const idx = this.players.findIndex((p) => p.bus_name === track.bus_name);
		if (idx >= 0) {
			const next = [...this.players];
			next[idx] = track;
			this.players = next;
		} else {
			this.players = [...this.players, track];
		}
	};

	/**
	 * Start the status poll: 5s cadence when something is playing/paused, 30s when
	 * idle. Returns a disposer that stops the loop. Call once from onMount.
	 */
	start = (): (() => void) => {
		const visible = () => typeof document === "undefined" || document.visibilityState === "visible";

		const scheduleNext = () => {
			// The launcher is hidden ~99% of its life, and the status pill isn't on
			// screen then — so don't poll MPRIS over D-Bus while hidden. Keep a slow
			// heartbeat (in case a visibilitychange is ever missed) but do the real
			// poll only when visible; showing the window fires an immediate poll. (FE-8)
			const hasActive = this.players.some((p) => p.status === "playing" || p.status === "paused");
			const delay = !visible() ? 60000 : hasActive ? 5000 : 30000;
			this.pollTimer = setTimeout(poll, delay);
		};

		const poll = async () => {
			if (!visible()) {
				scheduleNext();
				return;
			}
			try {
				const players = await mediaGetStatus();
				this.players = players.filter((p) => p.title || p.status !== "stopped");
			} catch {
				this.players = [];
			}
			scheduleNext();
		};

		// On show, poll immediately so the pill is fresh the instant the launcher
		// appears rather than up to 30s stale.
		const onVis = () => {
			if (visible()) {
				clearTimeout(this.pollTimer);
				poll();
			}
		};
		if (typeof document !== "undefined") {
			document.addEventListener("visibilitychange", onVis);
		}

		poll();
		return () => {
			clearTimeout(this.pollTimer);
			if (typeof document !== "undefined") {
				document.removeEventListener("visibilitychange", onVis);
			}
		};
	};
}

/** The single app-wide media state. */
export const media = new MediaState();

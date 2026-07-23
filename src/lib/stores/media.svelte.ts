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
		const scheduleNext = () => {
			const hasActive = this.players.some((p) => p.status === "playing" || p.status === "paused");
			this.pollTimer = setTimeout(poll, hasActive ? 5000 : 30000);
		};

		const poll = async () => {
			try {
				const players = await mediaGetStatus();
				this.players = players.filter((p) => p.title || p.status !== "stopped");
			} catch {
				this.players = [];
			}
			scheduleNext();
		};

		poll();
		return () => clearTimeout(this.pollTimer);
	};
}

/** The single app-wide media state. */
export const media = new MediaState();

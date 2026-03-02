<script lang="ts">
import type { TimerStatus } from "$lib/ipc";
import { executeCommand, getTimers } from "$lib/ipc";

let {
	timers = $bindable(),
	active = false,
}: {
	timers: TimerStatus[];
	active: boolean;
} = $props();

let timerInput = $state("");
let timerPollInterval: ReturnType<typeof setInterval> | undefined;

// Poll timers every 100ms when tab is active
$effect(() => {
	if (active) {
		async function poll() {
			try {
				timers = await getTimers();
			} catch {
				/* ignore */
			}
		}
		poll();
		timerPollInterval = setInterval(poll, 100);
	} else {
		clearInterval(timerPollInterval);
	}
	return () => clearInterval(timerPollInterval);
});

function formatTimerTime(secs: number): string {
	const total = Math.ceil(secs);
	if (total >= 3600) {
		const h = Math.floor(total / 3600);
		const m = Math.floor((total % 3600) / 60);
		const s = total % 60;
		return `${h}:${m.toString().padStart(2, "0")}:${s.toString().padStart(2, "0")}`;
	}
	const m = Math.floor(total / 60);
	const s = total % 60;
	return `${m}:${s.toString().padStart(2, "0")}`;
}

function formatTimerDuration(secs: number): string {
	const total = Math.ceil(secs);
	if (total >= 3600) {
		const h = Math.floor(total / 3600);
		const m = Math.floor((total % 3600) / 60);
		return m > 0 ? `${h}h ${m}m` : `${h}h`;
	}
	if (total >= 60) {
		const m = Math.floor(total / 60);
		const s = total % 60;
		return s > 0 ? `${m}m ${s}s` : `${m}m`;
	}
	return `${total}s`;
}

function timerProgress(t: TimerStatus): number {
	if (t.duration_secs <= 0) return 0;
	return Math.min(1, t.elapsed_secs / t.duration_secs);
}

async function handleCreateTimer() {
	const text = timerInput.trim();
	if (!text) return;
	try {
		await executeCommand(`timer ${text}`);
		timerInput = "";
	} catch (err) {
		console.error("[timer] create error:", err);
	}
}

async function handleTimerPause(name: string) {
	await executeCommand(`timer pause ${name}`);
}
async function handleTimerResume(name: string) {
	await executeCommand(`timer resume ${name}`);
}
async function handleTimerStop(name: string) {
	await executeCommand(`timer stop ${name}`);
}
</script>

<div class="section timer-section">
	<div class="timer-add">
		<input
			class="timer-input"
			bind:value={timerInput}
			onkeydown={(e) => { if (e.key === "Enter") { e.preventDefault(); e.stopPropagation(); handleCreateTimer(); } }}
			placeholder="25m, 1h30m, stopwatch..."
			type="text"
		/>
	</div>
	{#if timers.length === 0}
		<div class="timer-empty">No active timers. Add one above.</div>
	{:else}
		{#each timers as t (t.id)}
			<div class="timer-row" class:done={t.done}>
				<div class="timer-header">
					<span class="timer-name">
						{t.name}
						<span class="timer-badge" class:stopwatch={t.stopwatch}>
							{t.stopwatch ? "stopwatch" : "timer"}
						</span>
					</span>
					<span class="timer-remaining">
						{#if t.stopwatch}
							{#if t.paused}<span class="timer-paused-label">PAUSED</span>{/if}
							{formatTimerTime(t.elapsed_secs)}
						{:else if t.done}
							<span class="timer-done-label">DONE</span>
						{:else if t.paused}
							<span class="timer-paused-label">PAUSED</span>
							{formatTimerTime(t.remaining_secs)}
						{:else}
							{formatTimerTime(t.remaining_secs)}
						{/if}
					</span>
				</div>
				{#if !t.stopwatch}
					<div class="timer-progress-bar">
						<div
							class="timer-progress-fill"
							class:done={t.done}
							class:paused={t.paused}
							style="width: {timerProgress(t) * 100}%"
						></div>
					</div>
				{/if}
				<div class="timer-meta">
					<span class="timer-duration">
						{#if t.stopwatch}
							{formatTimerDuration(t.elapsed_secs)} elapsed
						{:else}
							{formatTimerDuration(t.duration_secs)}
						{/if}
					</span>
					<div class="timer-controls">
						{#if !t.done}
							{#if t.paused}
								<button class="timer-ctrl" onclick={() => handleTimerResume(t.name)} onmousedown={(e) => e.preventDefault()} tabindex={-1}>Resume</button>
							{:else}
								<button class="timer-ctrl" onclick={() => handleTimerPause(t.name)} onmousedown={(e) => e.preventDefault()} tabindex={-1}>Pause</button>
							{/if}
						{/if}
						<button class="timer-ctrl stop" onclick={() => handleTimerStop(t.name)} onmousedown={(e) => e.preventDefault()} tabindex={-1}>Stop</button>
					</div>
				</div>
			</div>
		{/each}
	{/if}
</div>

<style>
	.section {
		padding: 10px 20px;
	}

	.timer-add {
		margin-bottom: 4px;
	}

	.timer-input {
		width: 100%;
		background: var(--bg-secondary);
		border: 1px solid var(--border);
		border-radius: 6px;
		color: var(--fg);
		font-family: var(--font-mono);
		font-size: 13px;
		padding: 6px 10px;
		outline: none;
	}

	.timer-input:focus {
		border-color: var(--fg-muted);
	}

	.timer-input::placeholder {
		color: var(--fg-muted);
		opacity: 0.5;
	}

	.timer-section {
		display: flex;
		flex-direction: column;
		gap: 10px;
	}

	.timer-empty {
		font-family: var(--font-mono);
		font-size: 12px;
		color: var(--fg-muted);
		text-align: center;
		padding: 16px 0;
	}

	.timer-row {
		display: flex;
		flex-direction: column;
		gap: 5px;
	}

	.timer-row + .timer-row {
		padding-top: 10px;
		border-top: 1px solid var(--border);
	}

	.timer-header {
		display: flex;
		justify-content: space-between;
		align-items: baseline;
	}

	.timer-name {
		font-family: var(--font-mono);
		font-size: 13px;
		font-weight: 600;
		color: var(--fg);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		display: flex;
		align-items: center;
		gap: 6px;
	}

	.timer-badge {
		font-family: var(--font-mono);
		font-size: 9px;
		font-weight: 500;
		text-transform: uppercase;
		letter-spacing: 0.04em;
		padding: 1px 5px;
		border-radius: 3px;
		background: var(--bg);
		color: var(--fg-muted);
		border: 1px solid var(--border);
		flex-shrink: 0;
	}

	.timer-badge.stopwatch {
		color: var(--accent);
		border-color: var(--accent);
		opacity: 0.7;
	}

	.timer-remaining {
		font-family: var(--font-mono);
		font-size: 18px;
		font-weight: 600;
		color: var(--fg);
		font-variant-numeric: tabular-nums;
		flex-shrink: 0;
	}

	.timer-done-label {
		font-size: 11px;
		font-weight: 600;
		color: var(--success, #4caf50);
	}

	.timer-paused-label {
		font-size: 10px;
		font-weight: 500;
		color: var(--fg-muted);
		margin-right: 6px;
	}

	.timer-progress-bar {
		height: 3px;
		background: var(--bg);
		border-radius: 2px;
		overflow: hidden;
	}

	.timer-progress-fill {
		height: 100%;
		background: var(--fg);
		border-radius: 2px;
		transition: width 100ms linear;
	}

	.timer-progress-fill.paused {
		background: var(--fg-muted);
	}

	.timer-progress-fill.done {
		background: var(--success, #4caf50);
	}

	.timer-meta {
		display: flex;
		justify-content: space-between;
		align-items: center;
	}

	.timer-duration {
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--fg-muted);
	}

	.timer-controls {
		display: flex;
		gap: 6px;
	}

	.timer-ctrl {
		font-family: var(--font-mono);
		font-size: 10px;
		padding: 2px 8px;
		border-radius: 3px;
		border: 1px solid var(--border);
		background: transparent;
		color: var(--fg-muted);
		cursor: pointer;
		transition: color 100ms ease, background 100ms ease;
	}

	.timer-ctrl:hover {
		color: var(--fg);
		background: var(--bg);
	}

	.timer-ctrl.stop:hover {
		color: var(--error);
		border-color: var(--error);
	}
</style>

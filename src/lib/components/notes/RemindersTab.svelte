<script lang="ts">
import type { ReminderItem } from "$lib/ipc";
import { deleteReminder, executeCommand, getReminders } from "$lib/ipc";

let {
	reminders = $bindable(),
	active = false,
}: {
	reminders: ReminderItem[];
	active: boolean;
} = $props();

let reminderInput = $state("");
let reminderRefreshTimer: ReturnType<typeof setInterval> | undefined;

let activeReminders = $derived(reminders.filter((r) => !r.fired));
let firedReminders = $derived(reminders.filter((r) => r.fired));

// Auto-refresh reminders countdown every 10s when tab is active
$effect(() => {
	if (active) {
		reminderRefreshTimer = setInterval(() => {
			getReminders()
				.then((r) => {
					reminders = r;
				})
				.catch(() => {});
		}, 10_000);
	} else {
		clearInterval(reminderRefreshTimer);
	}
	return () => clearInterval(reminderRefreshTimer);
});

function formatReminderTime(dueAt: number): string {
	const now = Date.now();
	if (dueAt <= now) return "now";
	const diffSecs = Math.floor((dueAt - now) / 1000);
	if (diffSecs < 60) return `${diffSecs}s`;
	if (diffSecs < 3600) {
		const m = Math.floor(diffSecs / 60);
		const s = diffSecs % 60;
		return s > 0 ? `${m}m ${s}s` : `${m}m`;
	}
	if (diffSecs < 86400) {
		const h = Math.floor(diffSecs / 3600);
		const m = Math.floor((diffSecs % 3600) / 60);
		return m > 0 ? `${h}h ${m}m` : `${h}h`;
	}
	const d = Math.floor(diffSecs / 86400);
	const h = Math.floor((diffSecs % 86400) / 3600);
	return h > 0 ? `${d}d ${h}h` : `${d}d`;
}

function formatReminderDueLabel(dueAt: number): string {
	const date = new Date(dueAt);
	const now = new Date();
	const isToday = date.toDateString() === now.toDateString();
	const tomorrow = new Date(now);
	tomorrow.setDate(tomorrow.getDate() + 1);
	const isTomorrow = date.toDateString() === tomorrow.toDateString();
	const hours = date.getHours();
	const minutes = date.getMinutes();
	const ampm = hours >= 12 ? "pm" : "am";
	const h12 = hours % 12 || 12;
	const time =
		minutes > 0 ? `${h12}:${minutes.toString().padStart(2, "0")}${ampm}` : `${h12}${ampm}`;
	if (isToday) return `today ${time}`;
	if (isTomorrow) return `tomorrow ${time}`;
	const month = date.toLocaleString("en", { month: "short" }).toLowerCase();
	return `${month} ${date.getDate()} ${time}`;
}

function reminderProgress(rem: ReminderItem): number {
	const now = Date.now();
	if (rem.fired || now >= rem.due_at) return 1;
	const total = rem.due_at - rem.created_at;
	if (total <= 0) return 1;
	const elapsed = now - rem.created_at;
	return Math.max(0, Math.min(1, elapsed / total));
}

async function handleAddReminder() {
	const text = reminderInput.trim();
	if (!text) return;
	try {
		await executeCommand(`reminder add ${text}`);
		reminderInput = "";
		reminders = await getReminders();
	} catch (err) {
		console.error("[reminders] add error:", err);
	}
}

async function handleDeleteReminder(id: string) {
	try {
		await deleteReminder(id);
		reminders = reminders.filter((r) => r.id !== id);
	} catch (err) {
		console.error("[reminders] delete error:", err);
	}
}
</script>

<div class="section reminder-section">
	<div class="reminder-add">
		<input
			class="reminder-input"
			bind:value={reminderInput}
			onkeydown={(e) => { if (e.key === "Enter") { e.preventDefault(); e.stopPropagation(); handleAddReminder(); } }}
			placeholder="buy milk in 30m, standup at 9am..."
			type="text"
		/>
	</div>
	{#if activeReminders.length > 0 || firedReminders.length > 0}
		{#each activeReminders as rem (rem.id)}
			<div class="reminder-row">
				<div class="reminder-header">
					<span class="reminder-name">
						{rem.text}
					</span>
					<span class="reminder-countdown">
						{formatReminderTime(rem.due_at)}
					</span>
				</div>
				<div class="reminder-progress-bar">
					<div
						class="reminder-progress-fill"
						style="width: {reminderProgress(rem) * 100}%"
					></div>
				</div>
				<div class="reminder-meta">
					<span class="reminder-due">{formatReminderDueLabel(rem.due_at)}</span>
					<button
						class="reminder-ctrl stop"
						onclick={() => handleDeleteReminder(rem.id)}
						onmousedown={(e) => e.preventDefault()}
						tabindex={-1}
					>Dismiss</button>
				</div>
			</div>
		{/each}
		{#if firedReminders.length > 0}
			{#if activeReminders.length > 0}
				<div class="reminder-separator">
					<span class="reminder-separator-line"></span>
					<span class="reminder-separator-text">completed</span>
					<span class="reminder-separator-line"></span>
				</div>
			{/if}
			{#each firedReminders as rem (rem.id)}
				<div class="reminder-row fired">
					<div class="reminder-header">
						<span class="reminder-name">{rem.text}</span>
						<span class="reminder-fired-label">DONE</span>
					</div>
					<div class="reminder-meta">
						<span class="reminder-due">{formatReminderDueLabel(rem.due_at)}</span>
						<button
							class="reminder-ctrl stop"
							onclick={() => handleDeleteReminder(rem.id)}
							onmousedown={(e) => e.preventDefault()}
							tabindex={-1}
						>Remove</button>
					</div>
				</div>
			{/each}
		{/if}
	{:else}
		<div class="reminder-empty">No reminders. Add one above.</div>
	{/if}
</div>

<style>
	.section {
		padding: 10px 20px;
	}

	.reminder-section {
		display: flex;
		flex-direction: column;
		gap: 10px;
	}

	.reminder-add {
		margin-bottom: 0;
	}

	.reminder-input {
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

	.reminder-input:focus {
		border-color: var(--fg-muted);
	}

	.reminder-input::placeholder {
		color: var(--fg-muted);
		opacity: 0.5;
	}

	.reminder-row {
		display: flex;
		flex-direction: column;
		gap: 5px;
	}

	.reminder-row + .reminder-row {
		padding-top: 10px;
		border-top: 1px solid var(--border);
	}

	.reminder-row.fired {
		opacity: 0.5;
	}

	.reminder-row.fired .reminder-name {
		text-decoration: line-through;
		color: var(--fg-muted);
	}

	.reminder-header {
		display: flex;
		justify-content: space-between;
		align-items: baseline;
	}

	.reminder-name {
		font-family: var(--font-mono);
		font-size: 13px;
		font-weight: 600;
		color: var(--fg);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		flex: 1;
		min-width: 0;
	}

	.reminder-countdown {
		font-family: var(--font-mono);
		font-size: 18px;
		font-weight: 600;
		color: var(--fg);
		font-variant-numeric: tabular-nums;
		flex-shrink: 0;
		margin-left: 8px;
	}

	.reminder-fired-label {
		font-family: var(--font-mono);
		font-size: 11px;
		font-weight: 600;
		color: var(--success, #4caf50);
		flex-shrink: 0;
	}

	.reminder-progress-bar {
		height: 3px;
		background: var(--bg);
		border-radius: 2px;
		overflow: hidden;
	}

	.reminder-progress-fill {
		height: 100%;
		background: var(--fg);
		border-radius: 2px;
		transition: width 200ms linear;
	}

	.reminder-meta {
		display: flex;
		justify-content: space-between;
		align-items: center;
	}

	.reminder-due {
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--fg-muted);
	}

	.reminder-ctrl {
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

	.reminder-ctrl:hover {
		color: var(--fg);
		background: var(--bg);
	}

	.reminder-ctrl.stop:hover {
		color: var(--error);
		border-color: var(--error);
	}

	.reminder-separator {
		display: flex;
		align-items: center;
		gap: 8px;
		padding: 2px 0;
	}

	.reminder-separator-line {
		flex: 1;
		height: 1px;
		background: var(--border);
	}

	.reminder-separator-text {
		font-family: var(--font-mono);
		font-size: 9px;
		text-transform: uppercase;
		letter-spacing: 0.06em;
		color: var(--fg-muted);
		opacity: 0.6;
	}

	.reminder-empty {
		font-family: var(--font-mono);
		font-size: 12px;
		color: var(--fg-muted);
		text-align: center;
		padding: 16px 0;
	}
</style>

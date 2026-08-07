<script lang="ts">
import { Archive, FolderOpen, RotateCcw, Save, Trash2 } from "lucide-svelte";
import type { BackupInfo } from "$lib/ipc";
import {
	appVersionString,
	backupsDir,
	createBackup,
	deleteBackup,
	listBackups,
	restoreBackup,
	revealPath,
} from "$lib/ipc";

let { onsaveerror }: { onsaveerror: (msg: string) => void } = $props();

let backups: BackupInfo[] = $state([]);
let appVersion = $state("");
let dir = $state("");
let busy = $state(false);
/** Name of the backup awaiting confirmation. Restore replaces live data, so it
 *  is never one click — the confirm is inline on the row, not a modal, so the
 *  user can still see what they are about to overwrite. */
let confirmingRestore = $state<string | null>(null);
let confirmingDelete = $state<string | null>(null);
let status = $state("");

async function refresh() {
	[backups, appVersion, dir] = await Promise.all([listBackups(), appVersionString(), backupsDir()]);
}

$effect(() => {
	refresh().catch((err) => onsaveerror(`Couldn't list backups: ${err}`));
});

async function run(label: string, fn: () => Promise<string>) {
	if (busy) return;
	busy = true;
	status = `${label}…`;
	try {
		status = await fn();
		await refresh();
	} catch (err) {
		status = "";
		onsaveerror(`${label} failed: ${err}`);
	} finally {
		busy = false;
	}
}

const backUpNow = () =>
	run("Backing up", async () => {
		const b = await createBackup("manual backup");
		return `Saved ${b.name}`;
	});

const doRestore = (name: string) =>
	run("Restoring", async () => {
		confirmingRestore = null;
		const r = await restoreBackup(name);
		// Name the safety backup: the user just overwrote their data and the
		// single most useful thing to tell them is how to undo it.
		return `Restored ${r.rows_restored} rows. Previous state saved as ${r.safety_backup}`;
	});

const doDelete = (name: string) =>
	run("Deleting", async () => {
		confirmingDelete = null;
		await deleteBackup(name);
		return "Deleted";
	});

/** Bytes → a short human size. */
function size(bytes: number): string {
	if (bytes < 1024) return `${bytes} B`;
	if (bytes < 1024 * 1024) return `${Math.round(bytes / 1024)} KB`;
	return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

/** Unix millis → local date/time, or "" when unknown. */
function when(ms: number | undefined): string {
	if (!ms) return "";
	return new Date(ms).toLocaleString(undefined, {
		dateStyle: "medium",
		timeStyle: "short",
	});
}

function rows(b: BackupInfo): number {
	return (b.manifest?.tables ?? []).reduce(
		(n: number, [, count]: [string, number]) => n + count,
		0,
	);
}

/** Mirrors `BackupInfo::is_restorable` on the Rust side: an archive with no
 *  readable manifest, or one from a newer Lychi, must not be offered. */
function restorable(b: BackupInfo): boolean {
	const m = b.manifest;
	if (!m) return false;
	return !isNewer(m.app_version, appVersion);
}

function isNewer(a: string, b: string): boolean {
	const parse = (v: string) =>
		v
			.trim()
			.replace(/^v/, "")
			.split(/[-+]/)[0]
			.split(".")
			.map((n) => Number.parseInt(n, 10));
	const [x, y] = [parse(a), parse(b)];
	if (x.some(Number.isNaN) || y.some(Number.isNaN)) return false;
	for (let i = 0; i < 3; i++) {
		if ((x[i] ?? 0) !== (y[i] ?? 0)) return (x[i] ?? 0) > (y[i] ?? 0);
	}
	return false;
}
</script>

<div class="tab">
	<section>
		<div class="head">
			<div>
				<h3>Backups</h3>
				<p class="hint">
					Your history, clipboard, notes, snippets, settings and scripts. One is taken
					automatically before every upgrade.
				</p>
			</div>
			<button class="primary" onclick={backUpNow} disabled={busy}>
				<Save size={13} /> Back up now
			</button>
		</div>

		{#if status}
			<p class="status">{status}</p>
		{/if}

		{#if backups.length === 0}
			<p class="empty">No backups yet.</p>
		{:else}
			<div class="list">
				{#each backups as b (b.name)}
					<div class="row" class:unusable={!restorable(b)}>
						<Archive size={14} />
						<div class="meta">
							<span class="when">
								{when(b.manifest?.created_at)}
								{#if b.manifest?.kind === "automatic"}<span class="tag">auto</span>{/if}
							</span>
							<span class="detail">
								{#if b.manifest}
									{rows(b).toLocaleString()} rows · {size(b.size_bytes)}
									{#if b.manifest.reason}· {b.manifest.reason}{/if}
								{:else}
									Unreadable archive · {size(b.size_bytes)}
								{/if}
							</span>
						</div>

						<div class="actions">
							{#if confirmingRestore === b.name}
								<span class="warn">Replace current data?</span>
								<button class="danger" onclick={() => doRestore(b.name)} disabled={busy}>
									Replace
								</button>
								<button onclick={() => (confirmingRestore = null)}>Cancel</button>
							{:else if confirmingDelete === b.name}
								<span class="warn">Delete this backup?</span>
								<button class="danger" onclick={() => doDelete(b.name)} disabled={busy}>
									Delete
								</button>
								<button onclick={() => (confirmingDelete = null)}>Cancel</button>
							{:else}
								<button
									title={restorable(b)
										? "Restore this backup"
										: "Made by a newer Lychi — update to restore it"}
									disabled={busy || !restorable(b)}
									onclick={() => {
										confirmingDelete = null;
										confirmingRestore = b.name;
									}}
								>
									<RotateCcw size={12} /> Restore
								</button>
								<button
									class="icon"
									title="Delete this backup"
									disabled={busy}
									onclick={() => {
										confirmingRestore = null;
										confirmingDelete = b.name;
									}}
								>
									<Trash2 size={12} />
								</button>
							{/if}
						</div>
					</div>
				{/each}
			</div>
		{/if}

		{#if dir}
			<button class="link" onclick={() => revealPath(dir)}>
				<FolderOpen size={12} /> Open backups folder
			</button>
		{/if}
	</section>
</div>

<style>
	.tab {
		display: flex;
		flex-direction: column;
		gap: 16px;
	}

	.head {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: 12px;
	}

	h3 {
		margin: 0 0 2px;
		font-size: 13px;
		font-weight: 600;
	}

	.hint,
	.empty {
		margin: 0;
		font-size: 11px;
		color: var(--fg-muted);
		max-width: 46ch;
		line-height: 1.5;
	}

	.empty {
		padding: 12px 0;
	}

	.status {
		margin: 0;
		font-size: 11px;
		font-family: var(--font-mono);
		color: var(--accent);
	}

	.list {
		display: flex;
		flex-direction: column;
		gap: 1px;
		max-height: 260px;
		overflow-y: auto;
	}

	.row {
		display: flex;
		align-items: center;
		gap: 8px;
		padding: 6px 8px;
		border-radius: 3px;
		color: var(--fg);
	}

	.row:hover {
		background: var(--bg-secondary);
	}

	.row.unusable {
		color: var(--fg-muted);
	}

	.meta {
		display: flex;
		flex-direction: column;
		gap: 1px;
		min-width: 0;
		flex: 1;
	}

	.when {
		font-size: 12px;
		display: flex;
		align-items: center;
		gap: 6px;
	}

	.tag {
		font-size: 9px;
		text-transform: uppercase;
		letter-spacing: 0.04em;
		padding: 1px 4px;
		border-radius: 2px;
		background: var(--bg-secondary);
		color: var(--fg-muted);
	}

	.detail {
		font-size: 10px;
		color: var(--fg-muted);
		font-family: var(--font-mono);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	.actions {
		display: flex;
		align-items: center;
		gap: 4px;
		flex-shrink: 0;
	}

	.warn {
		font-size: 11px;
		color: var(--fg-muted);
	}

	button {
		display: inline-flex;
		align-items: center;
		gap: 4px;
		padding: 3px 8px;
		background: none;
		border: 1px solid var(--border);
		border-radius: 3px;
		color: var(--fg);
		font-size: 11px;
		font-family: inherit;
		cursor: pointer;
	}

	button:hover:not(:disabled) {
		border-color: var(--accent);
	}

	button:disabled {
		opacity: 0.4;
		cursor: default;
	}

	button.primary {
		color: var(--accent);
		flex-shrink: 0;
	}

	button.danger {
		color: var(--error);
		border-color: var(--error);
	}

	button.icon {
		padding: 3px 5px;
		color: var(--fg-muted);
	}

	button.icon:hover:not(:disabled) {
		color: var(--error);
		border-color: var(--error);
	}

	button.link {
		align-self: flex-start;
		border: none;
		padding: 0;
		color: var(--fg-muted);
		font-size: 11px;
	}

	button.link:hover {
		color: var(--accent);
	}
</style>

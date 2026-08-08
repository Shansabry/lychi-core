<script lang="ts">
import { getDiagnostics, getSetupChecklist, installCliLink, type SetupStepDto } from "$lib/ipc";

/// Setup — what this machine still needs, and what it already has.
///
/// A **diagnostics surface, not an onboarding checklist**. Every row is a fact
/// that can regress: replacing the AppImage dangles the CLI link, switching from
/// X11 to Wayland breaks a working hotkey. So nothing is ever marked done
/// forever, nothing is persisted, and the list is re-read each time it opens.
///
/// The backend decides everything — which rows exist, what they say, and what to
/// do about them. This component renders and dispatches; it holds no copy table
/// and no rules of its own, because a second opinion about "not applicable" is
/// exactly where a diagnostic turns into nagging.
let {
	onopentab,
}: {
	/// Route to the settings tab that already owns a control. Setup never
	/// duplicates one.
	onopentab: (tab: string) => void;
} = $props();

let steps = $state<SetupStepDto[]>([]);
let loading = $state(true);
let busy = $state<string | null>(null);
/// Announced politely to screen readers, since the visible change is a row
/// several lines away from the button that was pressed.
let announcement = $state("");
let copied = $state<string | null>(null);

/// Rows the user can act on. Drives whether Repair is offered at all — a
/// healthy machine has nothing to repair, and a button that does nothing
/// invites pressing it to find out.
let actionable = $derived(steps.filter((s) => s.state === "actionable"));
/// Rows we can fix without the user leaving this tab. `open_tab` rows are
/// excluded: those need a decision (which hotkey, which provider), and deciding
/// on someone's behalf is not repair.
let repairable = $derived(actionable.filter((s) => s.action === "install_cli"));

async function refresh() {
	try {
		const list = await getSetupChecklist();
		steps = list.steps;
	} catch (err) {
		console.error("[setup] failed to read checklist:", err);
	} finally {
		loading = false;
	}
}

// Probes D-Bus and may run the login shell, so this happens when the tab is
// first shown — never on the startup path.
refresh();

async function recheck() {
	busy = "recheck";
	announcement = "";
	try {
		await refresh();
		announcement = "Re-checked.";
	} finally {
		busy = null;
	}
}

async function runInstall(id: string) {
	busy = id;
	try {
		announcement = await installCliLink();
	} catch (err) {
		// Includes the linked-but-unreachable case, which is deliberately not
		// reported as success: a link the shell cannot see is not a command.
		announcement = String(err);
	} finally {
		busy = null;
		await refresh();
	}
}

/// Re-run every fix this tab can perform on its own.
///
/// Deliberately narrow: it repeats the same actions the individual rows offer
/// and nothing more. It does not reset settings, clear the database, or touch
/// anything the user configured — those live in Data, behind backup/restore,
/// and a button here that quietly did them would be the worst kind of surprise.
///
/// It also inherits every refusal from the underlying commands, so an existing
/// `lychi` somewhere else on PATH is reported, never replaced.
async function repairAll() {
	busy = "repair";
	const results: string[] = [];
	try {
		for (const step of repairable) {
			try {
				results.push(await installCliLink());
			} catch (err) {
				results.push(String(err));
			}
		}
		announcement = results.length ? results.join("\n") : "Nothing to repair automatically.";
	} finally {
		busy = null;
		await refresh();
	}
}

async function copyDiagnostics() {
	busy = "diagnostics";
	try {
		const report = await getDiagnostics();
		await navigator.clipboard.writeText(report);
		copied = "diagnostics";
		announcement = "Diagnostics copied — paste them into a bug report.";
		setTimeout(() => {
			copied = null;
		}, 1500);
	} catch (err) {
		console.error("[setup] diagnostics failed:", err);
		announcement = `Could not copy diagnostics: ${err}`;
	} finally {
		busy = null;
	}
}

async function copy(text: string, id: string) {
	try {
		await navigator.clipboard.writeText(text);
		copied = id;
		announcement = "Copied to clipboard.";
		setTimeout(() => {
			copied = null;
		}, 1500);
	} catch (err) {
		console.error("[setup] clipboard write failed:", err);
	}
}

/// Wording, not judgement — the state itself was decided in Rust.
function statusLabel(state: string): string {
	if (state === "done") return "Ready";
	if (state === "actionable") return "Action needed";
	return "Not applicable";
}
</script>

<div class="intro">Everything here is optional — Lychi works without it.</div>

{#if loading}
	<div class="empty">Checking…</div>
{:else if steps.length === 0}
	<div class="empty">Setup details are unavailable in the browser preview.</div>
{:else}
	<ul class="rows">
		{#each steps as step (step.id)}
			<li class="row" class:muted={step.state === "not_applicable"}>
				<!-- A marker only where it carries information. On a healthy machine
				     every row is `done`, so six green dots say nothing and read as
				     decoration; the eye should be drawn to the exception. -->
				<span
					class="dot"
					class:attention={step.state === "actionable"}
					aria-hidden="true"
				></span>
				<div class="text">
					<span class="title">
						{step.title}
						<!-- Status as text, never colour alone: the dot is invisible to a
						     screen reader and to a colourblind reader. -->
						<span class="sr-only">— {statusLabel(step.state)}</span>
					</span>
					<span class="detail">{step.detail}</span>
					{#if step.action === "manual" && step.command}
						<code class="cmd">{step.command}</code>
					{/if}
				</div>

				<!-- Only actionable rows get a control, so `done` and
				     `not_applicable` rows take no tab stop (GNOME boxed-list). -->
				{#if step.action === "install_cli"}
					<button
						class="act"
						onclick={() => runInstall(step.id)}
						disabled={busy === step.id}
					>
						{busy === step.id ? "Setting up…" : "Set up"}
					</button>
				{:else if step.action === "manual" && step.command}
					<button class="act" onclick={() => copy(step.command ?? "", step.id)}>
						{copied === step.id ? "Copied" : "Copy"}
					</button>
				{:else if step.action === "open_tab" && step.tab}
					<button class="act" onclick={() => onopentab(step.tab ?? "general")}>
						Open
					</button>
				{/if}
			</li>
		{/each}
	</ul>
{/if}

<div class="sr-only" role="status" aria-live="polite">{announcement}</div>
{#if announcement}
	<div class="announce">{announcement}</div>
{/if}

{#if !loading && steps.length > 0}
	<div class="footer">
		<button class="link" onclick={recheck} disabled={busy !== null}>
			{busy === "recheck" ? "Checking…" : "Re-check"}
		</button>
		<!-- Offered only when something is actually repairable. A permanent
		     Repair button on a healthy machine is an invitation to press it and
		     find out what it does. -->
		{#if repairable.length > 0}
			<button class="link" onclick={repairAll} disabled={busy !== null}>
				{busy === "repair" ? "Repairing…" : "Repair"}
			</button>
		{/if}
		<button class="link right" onclick={copyDiagnostics} disabled={busy !== null}>
			{copied === "diagnostics" ? "Copied" : "Copy diagnostics"}
		</button>
	</div>
	<div class="footer-hint">
		Diagnostics describe this desktop and what Lychi can use on it — paste them
		into a bug report.
	</div>
{/if}

<style>
	.intro {
		font-size: 11px;
		color: var(--fg-muted);
		opacity: 0.8;
		padding: 2px 0 12px;
		line-height: 1.5;
	}

	.empty {
		font-size: 12px;
		color: var(--fg-muted);
		padding: 12px 0;
	}

	.rows {
		list-style: none;
		margin: 0;
		padding: 0;
		display: flex;
		flex-direction: column;
	}

	.row {
		display: flex;
		align-items: flex-start;
		gap: 10px;
		padding: 11px 0;
		border-bottom: 1px solid color-mix(in srgb, var(--border) 55%, transparent);
	}
	.row:last-child {
		border-bottom: none;
	}
	.row.muted {
		opacity: 0.55;
	}

	/* Reserves the gutter so every title lines up whether or not a marker is
	   drawn — a row that shifts left when it turns healthy would be worse than
	   the dot it lost. */
	.dot {
		width: 7px;
		height: 7px;
		border-radius: 50%;
		background: transparent;
		flex-shrink: 0;
		margin-top: 5px;
	}
	.dot.attention {
		background: var(--accent);
		box-shadow: 0 0 7px color-mix(in srgb, var(--accent) 60%, transparent);
	}

	.text {
		display: flex;
		flex-direction: column;
		gap: 3px;
		min-width: 0;
		flex: 1;
	}

	.title {
		font-size: 12px;
		color: var(--fg);
	}

	.detail {
		font-size: 11px;
		color: var(--fg-muted);
		line-height: 1.45;
		word-break: break-word;
	}

	.cmd {
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--accent);
		background: var(--bg-secondary);
		border: 1px solid var(--border);
		border-radius: 5px;
		padding: 4px 7px;
		margin-top: 3px;
		align-self: flex-start;
		word-break: break-all;
	}

	.act {
		margin-left: auto;
		flex-shrink: 0;
		background: color-mix(in srgb, var(--bg-secondary) 60%, var(--bg));
		color: var(--accent);
		border: 1px solid color-mix(in srgb, var(--accent) 35%, transparent);
		border-radius: 7px;
		padding: 5px 11px;
		font-family: var(--font-mono);
		font-size: 11px;
		cursor: pointer;
		margin-top: 1px;
	}
	.act:hover:not(:disabled) {
		border-color: color-mix(in srgb, var(--accent) 55%, transparent);
	}
	.act:disabled {
		opacity: 0.5;
		cursor: default;
	}

	.announce {
		font-size: 11px;
		color: var(--fg-muted);
		line-height: 1.5;
		padding: 10px 0 0;
		white-space: pre-wrap;
		word-break: break-word;
	}

	/* Quieter than the row actions: these are things you reach for when
	   something is wrong, not part of the reading. */
	.footer {
		display: flex;
		align-items: center;
		gap: 14px;
		margin-top: 14px;
		padding-top: 12px;
		border-top: 1px solid color-mix(in srgb, var(--border) 60%, transparent);
	}

	.link {
		background: none;
		border: none;
		padding: 0;
		font-family: inherit;
		font-size: 11px;
		color: var(--fg-muted);
		cursor: pointer;
		text-decoration: underline;
		text-underline-offset: 3px;
		text-decoration-color: color-mix(in srgb, var(--fg-muted) 40%, transparent);
	}
	.link:hover:not(:disabled) {
		color: var(--fg);
	}
	.link:disabled {
		opacity: 0.5;
		cursor: default;
	}
	.link.right {
		margin-left: auto;
	}

	.footer-hint {
		font-size: 10px;
		color: var(--fg-muted);
		opacity: 0.7;
		padding-top: 6px;
		line-height: 1.5;
	}

	.sr-only {
		position: absolute;
		width: 1px;
		height: 1px;
		padding: 0;
		margin: -1px;
		overflow: hidden;
		clip: rect(0, 0, 0, 0);
		white-space: nowrap;
		border: 0;
	}
</style>

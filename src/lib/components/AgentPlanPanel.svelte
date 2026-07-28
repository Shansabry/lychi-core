<script lang="ts">
import type { AgentPlan, CommandResult, StepEvent } from "$lib/ipc";
import { executeAgentPlan, storeAgentPlan } from "$lib/ipc";

let {
	plan,
	onexecuted,
	ondismiss,
}: {
	plan: AgentPlan;
	onexecuted: () => void;
	ondismiss: () => void;
} = $props();

type StepStatus = "pending" | "running" | "done" | "failed";

// plan is immutable for the lifetime of this component — suppress state_referenced_locally
// svelte-ignore state_referenced_locally
let stepStatuses: StepStatus[] = $state(plan.steps.map((): StepStatus => "pending"));
// svelte-ignore state_referenced_locally
let stepResults: (CommandResult | null)[] = $state(plan.steps.map(() => null));
let executing = $state(false);
let done = $state(false);

// Steps that will prompt/deny at execution — surfaced up-front so the user
// approves an INFORMED plan, not just a natural-language summary. `medium`
// shell steps (rm on a real path, sudo, package installs) confirm; `high`
// steps are the AI's own danger flag or a denylist-adjacent command.
let riskyCount = $derived(
	plan.steps.filter((s) => s.risk === "medium" || s.risk === "high").length,
);

export function handleStepEvent(event: StepEvent) {
	if (event.plan_id !== plan.id) return;
	stepStatuses[event.step_index] = event.status as StepStatus;
	if (event.result) {
		stepResults[event.step_index] = event.result;
	}
	// Check if all done or a step failed
	if (event.status === "failed" || event.step_index === plan.steps.length - 1) {
		if (event.status === "done" || event.status === "failed") {
			executing = false;
			done = true;
		}
	}
}

async function execute() {
	if (executing || done) return;
	executing = true;
	await storeAgentPlan(plan);
	await executeAgentPlan(plan.id);
	onexecuted();
}

function handleKeydown(e: KeyboardEvent) {
	if (e.key === "Enter" && !executing && !done) {
		e.preventDefault();
		e.stopPropagation();
		execute();
	} else if (e.key === "Escape") {
		e.preventDefault();
		e.stopPropagation();
		ondismiss();
	}
}

function statusIcon(status: StepStatus): string {
	switch (status) {
		case "pending":
			return "○";
		case "running":
			return "⟳";
		case "done":
			return "✓";
		case "failed":
			return "✗";
	}
}
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="agent-plan" onkeydown={handleKeydown} tabindex="-1">
	<div class="plan-header">
		<span class="plan-icon">✦</span>
		<span>Plan ({plan.steps.length} steps)</span>
	</div>

	<div class="steps">
		{#each plan.steps as step, i}
			<div class="step" class:running={stepStatuses[i] === "running"} class:done={stepStatuses[i] === "done"} class:failed={stepStatuses[i] === "failed"} class:dangerous={step.risk === "high"} class:caution={step.risk === "medium"}>
				<div class="step-line">
					<span class="step-status" class:running={stepStatuses[i] === "running"} class:done={stepStatuses[i] === "done"} class:failed={stepStatuses[i] === "failed"}>
						{statusIcon(stepStatuses[i])}
					</span>
					<span class="step-prefix">{step.action_id}</span>
					<span class="step-args">{step.args}</span>
					{#if stepResults[i]?.duration_ms}
						<span class="step-duration">{stepResults[i]?.duration_ms}ms</span>
					{/if}
				</div>
				<div class="step-label" class:dangerous={step.risk === "high"} class:caution={step.risk === "medium"}>
					{step.label}
					{#if step.risk === "high"}
						<span class="risk-badge high">⚠ high risk</span>
					{:else if step.risk === "medium"}
						<span class="risk-badge medium">⚠ needs care</span>
					{/if}
				</div>
				{#if stepResults[i]?.error}
					<pre class="step-error">{stepResults[i]?.error}</pre>
				{/if}
			</div>
		{/each}
	</div>

	{#if !executing && !done}
		{#if riskyCount > 0}
			<div class="plan-warning">
				⚠ {riskyCount} step{riskyCount > 1 ? "s" : ""} can modify your system — review the commands above before running.
			</div>
		{/if}
		<div class="plan-actions">
			<span class="hint">Enter to execute</span>
			<span class="hint-sep">·</span>
			<span class="hint">Esc to cancel</span>
		</div>
	{/if}
</div>

<style>
	.agent-plan {
		padding: 12px 20px;
		max-height: 300px;
		overflow-y: auto;
		font-family: var(--font-mono);
		font-size: 13px;
		color: var(--fg);
	}

	.plan-header {
		display: flex;
		align-items: center;
		gap: 6px;
		font-size: 11px;
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: 0.05em;
		color: var(--fg-muted);
		margin-bottom: 10px;
	}

	.plan-icon {
		font-size: 10px;
		opacity: 0.6;
	}

	.steps {
		display: flex;
		flex-direction: column;
		gap: 8px;
	}

	.step {
		padding: 6px 8px;
		border-radius: 4px;
		background: var(--bg-secondary);
		border: 1px solid var(--border);
	}

	.step.running {
		border-color: var(--fg-muted);
	}

	.step.done {
		opacity: 0.7;
	}

	.step.failed {
		border-color: var(--error);
	}

	.step.dangerous {
		/* Dimmed amber: present enough to flag the row, not so loud it competes
		   with the confirm button. Derived from the token so it tracks the theme. */
		border-color: color-mix(in srgb, var(--warning) 45%, var(--border));
	}

	.step.caution {
		border-color: color-mix(in srgb, var(--warning-muted) 30%, var(--border));
	}

	.step-line {
		display: flex;
		align-items: center;
		gap: 6px;
	}

	.step-status {
		font-size: 12px;
		color: var(--fg-muted);
		flex-shrink: 0;
		width: 14px;
		text-align: center;
	}

	.step-status.running {
		color: var(--fg);
		animation: spin 1s linear infinite;
	}

	.step-status.done {
		color: var(--success);
	}

	.step-status.failed {
		color: var(--error);
	}

	@keyframes spin {
		from { transform: rotate(0deg); }
		to { transform: rotate(360deg); }
	}

	.step-prefix {
		color: var(--fg-muted);
		font-size: 11px;
		flex-shrink: 0;
	}

	.step-args {
		color: var(--fg);
		font-size: 12px;
		flex: 1;
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.step-duration {
		color: var(--fg-muted);
		font-size: 11px;
		flex-shrink: 0;
	}

	.step-label {
		font-size: 11px;
		color: var(--fg-muted);
		margin-top: 2px;
		padding-left: 20px;
	}

	.step-label.dangerous {
		color: var(--warning);
	}

	.step-label.caution {
		color: var(--warning-muted);
	}

	.risk-badge {
		font-size: 10px;
		margin-left: 4px;
	}

	.risk-badge.high {
		color: var(--warning);
	}

	.risk-badge.medium {
		color: var(--warning-muted);
	}

	.plan-warning {
		font-size: 11px;
		color: var(--warning-muted);
		background: color-mix(in srgb, var(--warning-muted) 8%, transparent);
		border: 1px solid color-mix(in srgb, var(--warning-muted) 25%, transparent);
		border-radius: 4px;
		padding: 6px 8px;
		margin-top: 10px;
		line-height: 1.4;
	}

	.step-error {
		font-size: 11px;
		color: var(--error);
		margin-top: 4px;
		padding-left: 20px;
		white-space: pre-wrap;
		word-break: break-word;
	}

	.plan-actions {
		display: flex;
		align-items: center;
		justify-content: center;
		gap: 8px;
		padding-top: 10px;
		margin-top: 6px;
		border-top: 1px solid var(--border);
	}

	.hint {
		font-size: 11px;
		color: var(--fg-muted);
	}

	.hint-sep {
		color: var(--fg-muted);
		opacity: 0.4;
	}
</style>

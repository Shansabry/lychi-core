<script lang="ts">
type Health = "checking" | "healthy" | "error" | "disabled";

let {
	health,
	testing,
	testResult,
	ontest,
}: {
	health: Health;
	testing: boolean;
	testResult: { ok: boolean; error: string | null } | null;
	ontest: () => void;
} = $props();
</script>

<div class="status-row">
	<span
		class="dot"
		class:healthy={health === "healthy"}
		class:error={health === "error"}
		class:checking={health === "checking"}
	></span>
	<span class="label">
		{#if health === "checking"}
			Checking…
		{:else if health === "healthy"}
			Connected
		{:else if health === "error"}
			Not connected
		{:else}
			Disabled
		{/if}
	</span>
	<button class="test-btn" onclick={ontest} disabled={testing}>
		{testing ? "Testing…" : "Test"}
		<span class="kbd">⌘↵</span>
	</button>
</div>
{#if testResult}
	<div class="result" class:ok={testResult.ok} class:fail={!testResult.ok}>
		{#if testResult.ok}
			✓ Connection OK — endpoint, key, and model all responded.
		{:else}
			✗ {testResult.error ?? "Connection failed."}
		{/if}
	</div>
{/if}

<style>
	.status-row {
		display: flex;
		align-items: center;
		gap: 8px;
		margin-top: 18px;
		padding-top: 14px;
		border-top: 1px solid color-mix(in srgb, var(--border) 60%, transparent);
	}
	.dot {
		width: 8px;
		height: 8px;
		border-radius: 50%;
		background: var(--fg-muted);
		flex-shrink: 0;
	}
	.dot.healthy {
		background: var(--success);
		box-shadow: 0 0 8px color-mix(in srgb, var(--success) 70%, transparent);
	}
	.dot.error {
		background: var(--error);
	}
	.dot.checking {
		background: var(--fg-muted);
		animation: pulse 1s ease-in-out infinite;
	}
	.label {
		font-size: 12px;
		color: var(--fg-muted);
	}
	.test-btn {
		margin-left: auto;
		display: inline-flex;
		align-items: center;
		gap: 7px;
		background: color-mix(in srgb, var(--bg-secondary) 60%, var(--bg));
		color: var(--accent);
		border: 1px solid color-mix(in srgb, var(--accent) 35%, transparent);
		border-radius: 7px;
		padding: 5px 11px;
		font-family: var(--font-mono);
		font-size: 12px;
		cursor: pointer;
	}
	.test-btn:hover:not(:disabled) {
		border-color: color-mix(in srgb, var(--accent) 55%, transparent);
	}
	.test-btn:disabled {
		opacity: 0.5;
		cursor: default;
	}
	.kbd {
		font-family: var(--font-mono);
		font-size: 10px;
		color: var(--fg-muted);
		background: var(--bg-secondary);
		border: 1px solid var(--border);
		border-radius: 4px;
		padding: 0 4px;
	}
	.result {
		font-size: 11px;
		padding: 8px 0 2px;
		line-height: 1.4;
		word-break: break-word;
	}
	.result.ok {
		color: var(--success);
	}
	.result.fail {
		color: var(--error);
	}
</style>

<script lang="ts">
type Health = "checking" | "healthy" | "error" | "disabled";

let {
	providerLabel,
	model,
	maskedKey,
	modeLabel,
	health,
	testing,
	onchangerequest,
	ontest,
}: {
	providerLabel: string;
	model: string;
	maskedKey: string | null;
	modeLabel: string;
	health: Health;
	testing: boolean;
	onchangerequest: () => void;
	ontest: () => void;
} = $props();

const subtitle = $derived(
	[model || "default model", maskedKey ? `key ${maskedKey}` : null, modeLabel]
		.filter(Boolean)
		.join(" · "),
);
</script>

<div class="summary">
	<div class="badge">✦</div>
	<div class="main">
		<div class="title">
			{providerLabel}
			<span
				class="pill"
				class:ok={health === "healthy"}
				class:bad={health === "error"}
				class:wait={health === "checking"}
			>
				<span class="led"></span>
				{#if health === "healthy"}
					Connected
				{:else if health === "error"}
					Not connected
				{:else if health === "checking"}
					Checking…
				{:else}
					Disabled
				{/if}
			</span>
		</div>
		<div class="sub">{subtitle}</div>
	</div>
	<button class="btn ai" onclick={ontest} disabled={testing}>
		{testing ? "Testing…" : "Test"}
		<span class="kbd">⌘↵</span>
	</button>
	<button class="btn ghost" onclick={onchangerequest}>Change</button>
</div>

<style>
	.summary {
		display: flex;
		align-items: center;
		gap: 14px;
		background: linear-gradient(
			180deg,
			color-mix(in srgb, var(--fg) 5%, var(--bg)),
			var(--bg-secondary)
		);
		border: 1px solid var(--border);
		border-radius: 10px;
		padding: 16px 18px;
	}
	.badge {
		width: 38px;
		height: 38px;
		border-radius: 9px;
		flex-shrink: 0;
		background: color-mix(in srgb, var(--ai) 16%, var(--bg-secondary));
		border: 1px solid color-mix(in srgb, var(--ai) 30%, transparent);
		display: grid;
		place-items: center;
		color: var(--ai);
		font-size: 17px;
	}
	.main {
		flex: 1;
		min-width: 0;
	}
	.title {
		font-size: 13.5px;
		font-weight: 600;
		color: var(--fg);
		display: flex;
		align-items: center;
		gap: 8px;
	}
	.sub {
		color: var(--fg-muted);
		font-size: 11.5px;
		margin-top: 3px;
		font-family: var(--font-mono);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.pill {
		display: inline-flex;
		align-items: center;
		gap: 5px;
		font-size: 10.5px;
		font-family: var(--font-mono);
		padding: 2px 8px;
		border-radius: 20px;
		border: 1px solid var(--border);
		color: var(--fg-muted);
		letter-spacing: 0.3px;
	}
	.pill.ok {
		color: var(--success);
		border-color: color-mix(in srgb, var(--success) 40%, transparent);
		background: color-mix(in srgb, var(--success) 10%, transparent);
	}
	.pill.bad {
		color: var(--error);
		border-color: color-mix(in srgb, var(--error) 40%, transparent);
	}
	.pill .led {
		width: 6px;
		height: 6px;
		border-radius: 50%;
		background: currentColor;
		box-shadow: 0 0 8px currentColor;
	}
	.pill.wait .led {
		animation: pulse 1s ease-in-out infinite;
	}
	.btn {
		font-family: var(--font-mono);
		font-size: 12px;
		color: var(--fg);
		background: color-mix(in srgb, var(--fg) 5%, var(--bg));
		border: 1px solid var(--border);
		border-radius: 7px;
		padding: 6px 12px;
		cursor: pointer;
		display: inline-flex;
		align-items: center;
		gap: 7px;
		flex-shrink: 0;
	}
	.btn:hover:not(:disabled) {
		border-color: var(--fg-muted);
	}
	.btn:disabled {
		opacity: 0.5;
		cursor: default;
	}
	.btn.ai {
		color: var(--ai);
		border-color: color-mix(in srgb, var(--ai) 35%, transparent);
	}
	.btn.ghost {
		background: transparent;
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
</style>

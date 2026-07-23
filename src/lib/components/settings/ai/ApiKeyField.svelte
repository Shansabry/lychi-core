<script lang="ts">
import { getMaskedApiKey, setApiKey } from "$lib/ipc";

let {
	provider,
	onhealthchange,
}: {
	provider: string;
	/** Called after the key is set/cleared so the parent can re-check health. */
	onhealthchange?: () => void;
} = $props();

let apiKeyInput = $state("");
let maskedKey: string | null = $state(null);
let editingKey = $state(false);
let confirmingClear = $state(false);
let saving = $state(false);

// Re-fetch the masked key whenever the provider changes (and on mount).
$effect(() => {
	const p = provider;
	apiKeyInput = "";
	editingKey = false;
	confirmingClear = false;
	getMaskedApiKey(p)
		.then((k) => {
			if (p === provider) maskedKey = k;
		})
		.catch(() => {
			if (p === provider) maskedKey = null;
		});
});

async function handleSetApiKey() {
	if (!apiKeyInput.trim()) return;
	saving = true;
	try {
		await setApiKey(provider, apiKeyInput.trim());
		apiKeyInput = "";
		editingKey = false;
		confirmingClear = false;
		maskedKey = await getMaskedApiKey(provider);
		onhealthchange?.();
	} finally {
		saving = false;
	}
}

async function handleClearApiKey() {
	if (!confirmingClear) {
		confirmingClear = true;
		return;
	}
	confirmingClear = false;
	saving = true;
	try {
		await setApiKey(provider, "");
		maskedKey = null;
		apiKeyInput = "";
		editingKey = false;
		onhealthchange?.();
	} finally {
		saving = false;
	}
}

/** Bubbled up so Esc can dismiss the "Sure?" confirm before closing settings. */
export function dismissConfirm(): boolean {
	if (confirmingClear) {
		confirmingClear = false;
		return true;
	}
	return false;
}
</script>

<div class="field">
	<label for="ai-key">API Key</label>
	<div class="key-row">
		{#if maskedKey && !editingKey}
			<span class="masked-key">{maskedKey}</span>
			<button
				class="set-btn"
				onclick={() => {
					editingKey = true;
					confirmingClear = false;
				}}
				disabled={saving}
			>
				Change
			</button>
			<button
				class="set-btn clear-btn"
				class:confirming={confirmingClear}
				onclick={handleClearApiKey}
				disabled={saving}
			>
				{confirmingClear ? "Sure?" : "Clear"}
			</button>
		{:else}
			<input
				id="ai-key"
				type="password"
				bind:value={apiKeyInput}
				placeholder="Enter API key…"
				spellcheck="false"
				onkeydown={(e) => {
					if (e.key === "Enter") {
						e.preventDefault();
						handleSetApiKey();
					}
				}}
			/>
			<button
				class="set-btn"
				onclick={handleSetApiKey}
				disabled={saving || !apiKeyInput.trim()}
			>
				Set
			</button>
			{#if maskedKey}
				<button
					class="set-btn"
					onclick={() => {
						editingKey = false;
						apiKeyInput = "";
						confirmingClear = false;
					}}
					disabled={saving}
				>
					Cancel
				</button>
			{/if}
		{/if}
	</div>
</div>

<style>
	.field {
		display: grid;
		grid-template-columns: 120px 1fr;
		align-items: center;
		gap: 14px;
		padding: 9px 0;
		border-top: 1px solid color-mix(in srgb, var(--border) 60%, transparent);
	}
	label {
		font-size: 12.5px;
		color: var(--fg-muted);
	}
	.key-row {
		display: flex;
		gap: 6px;
		min-width: 0;
		align-items: center;
	}
	.key-row input {
		flex: 1;
		min-width: 0;
		background: var(--bg-secondary);
		color: var(--fg);
		border: 1px solid var(--border);
		border-radius: 7px;
		padding: 8px 11px;
		font-family: var(--font-mono);
		font-size: 12.5px;
		outline: none;
	}
	.key-row input:focus {
		border-color: var(--fg-muted);
	}
	.masked-key {
		font-family: var(--font-mono);
		font-size: 12.5px;
		color: var(--fg-muted);
		flex: 1;
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.set-btn {
		background: var(--bg-secondary);
		color: var(--accent);
		border: 1px solid var(--border);
		border-radius: 7px;
		padding: 7px 12px;
		font-family: var(--font-mono);
		font-size: 12px;
		cursor: pointer;
		flex-shrink: 0;
		transition: background 100ms ease;
	}
	.set-btn:hover:not(:disabled) {
		background: var(--border);
	}
	.set-btn:disabled {
		opacity: 0.4;
		cursor: default;
	}
	.clear-btn:hover {
		color: var(--error);
		border-color: var(--error);
		background: var(--bg-secondary);
	}
	.clear-btn.confirming {
		color: var(--error);
		border-color: var(--error);
	}
</style>

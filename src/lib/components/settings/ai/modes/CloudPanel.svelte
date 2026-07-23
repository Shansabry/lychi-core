<script lang="ts">
import { listen } from "@tauri-apps/api/event";
import { onMount } from "svelte";
import type { CreditBalance, FirebaseUser } from "$lib/ipc";
import { cloudGetCredits, firebaseGetUser, firebaseSignIn, firebaseSignOut } from "$lib/ipc";

let {
	onsaveerror,
}: {
	onsaveerror: (msg: string) => void;
} = $props();

// Lychi Cloud is disabled for launch (BYOK + Ollama only) — flip this when
// lychi-cloud ships (Phase 2.3). Typed as boolean so TS doesn't narrow the
// gated markup to unreachable.
const CLOUD_ENABLED: boolean = false;

let cloudUser: FirebaseUser | null = $state(null);
let cloudCredits: CreditBalance | null = $state(null);
let cloudLoading = $state(false);

export async function refreshCloudUser() {
	try {
		cloudUser = await firebaseGetUser();
		if (cloudUser) {
			refreshCloudCredits();
		} else {
			cloudCredits = null;
		}
	} catch {
		cloudUser = null;
		cloudCredits = null;
	}
}

async function refreshCloudCredits() {
	try {
		cloudCredits = await cloudGetCredits();
	} catch {
		cloudCredits = null;
	}
}

async function handleCloudSignIn() {
	cloudLoading = true;
	try {
		await firebaseSignIn();
	} catch (err) {
		onsaveerror(`Sign in failed: ${err}`);
	} finally {
		cloudLoading = false;
	}
}

async function handleCloudSignOut() {
	cloudLoading = true;
	try {
		await firebaseSignOut();
		cloudUser = null;
		cloudCredits = null;
	} catch (err) {
		onsaveerror(`Sign out failed: ${err}`);
	} finally {
		cloudLoading = false;
	}
}

onMount(() => {
	if (CLOUD_ENABLED) refreshCloudUser();
	const unlistenSignIn = listen("lychi://firebase-signed-in", () => {
		refreshCloudUser();
	});
	const unlistenSignOut = listen("lychi://firebase-signed-out", () => {
		cloudUser = null;
		cloudCredits = null;
	});
	return () => {
		unlistenSignIn.then((u) => u());
		unlistenSignOut.then((u) => u());
	};
});
</script>

<div class="hint">
	Lychi Cloud isn't available yet — switch to Local AI, Ollama, or BYO API key.
</div>

{#if CLOUD_ENABLED}
	{#if cloudUser}
		<div class="field">
			<span class="label">Signed in as</span>
			<span class="email">{cloudUser.email}</span>
		</div>
		{#if cloudCredits}
			<div class="field">
				<span class="label">Credits</span>
				<div class="credit-info">
					<span class="credit-balance">{cloudCredits.balance.toLocaleString()}</span>
					<span class="credit-meta">/ {cloudCredits.plan} plan</span>
				</div>
			</div>
			{#if cloudCredits.bonus_pool > 0}
				<div class="field">
					<span class="label">Bonus pool</span>
					<span class="credit-meta">{cloudCredits.bonus_pool.toLocaleString()}</span>
				</div>
			{/if}
		{/if}
		<div class="field">
			<span class="label"></span>
			<button class="btn" onclick={handleCloudSignOut} disabled={cloudLoading}>
				Sign out
			</button>
		</div>
	{:else}
		<div class="field">
			<span class="label">Account</span>
			<button class="btn" onclick={handleCloudSignIn} disabled={cloudLoading}>
				{cloudLoading ? "Opening browser…" : "Sign in with Google"}
			</button>
		</div>
		<div class="hint">
			Opens your browser to sign in. You'll be redirected back to Lychi automatically.
		</div>
	{/if}
{/if}

<style>
	.hint {
		font-size: 12px;
		color: var(--fg-muted);
		line-height: 1.5;
		padding: 4px 0 10px;
	}
	.field {
		display: grid;
		grid-template-columns: 120px 1fr;
		align-items: center;
		gap: 14px;
		padding: 9px 0;
	}
	.label {
		font-size: 12.5px;
		color: var(--fg-muted);
	}
	.email {
		font-family: var(--font-mono);
		font-size: 12px;
		color: var(--fg);
	}
	.credit-info {
		display: flex;
		align-items: baseline;
		gap: 6px;
	}
	.credit-balance {
		font-family: var(--font-mono);
		font-size: 14px;
		color: var(--accent);
		font-weight: 600;
	}
	.credit-meta {
		font-size: 11px;
		color: var(--fg-muted);
	}
	.btn {
		background: var(--bg-secondary);
		color: var(--accent);
		border: 1px solid var(--border);
		border-radius: 7px;
		padding: 7px 12px;
		font-family: var(--font-mono);
		font-size: 12px;
		cursor: pointer;
		justify-self: start;
	}
	.btn:hover:not(:disabled) {
		background: var(--border);
	}
	.btn:disabled {
		opacity: 0.4;
		cursor: default;
	}
</style>

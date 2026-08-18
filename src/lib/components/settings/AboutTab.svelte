<script lang="ts">
import type { UpdateStatus } from "$lib/ipc";
import { checkForUpdate, installUpdate, openPath } from "$lib/ipc";
import LychiIcon from "../LychiIcon.svelte";

let { appVersion }: { appVersion: string } = $props();

// The update check is an EXPLICIT action — About never touches the network on
// its own. The click is the consent; the result is shown beside the pill.
let checking = $state(false);
let installing = $state(false);
let checked = $state<UpdateStatus | null>(null);

async function runCheck() {
	checking = true;
	try {
		checked = await checkForUpdate();
	} catch (e) {
		checked = null;
	} finally {
		checking = false;
	}
}

async function runInstall() {
	installing = true;
	try {
		// Backs up first, then installs and relaunches — this call only
		// returns on failure.
		await installUpdate();
	} finally {
		installing = false;
	}
}
</script>

<div class="about">
	<div class="about-version-row">
		<span class="about-version-pill">v{appVersion}</span>
		{#if checked?.available_version}
			<span class="about-update-found">v{checked.available_version} available</span>
			{#if checked.can_self_update}
				<button class="about-update-btn" onclick={runInstall} disabled={installing}>
					{installing ? "Installing…" : "Install & restart"}
				</button>
			{:else}
				<span class="about-update-hint">{checked.hint}</span>
			{/if}
		{:else if checked?.error}
			<span class="about-update-hint">{checked.error}</span>
		{:else if checked}
			<span class="about-update-ok">up to date</span>
		{:else}
			<button class="about-update-btn" onclick={runCheck} disabled={checking}>
				{checking ? "Checking…" : "Check for updates"}
			</button>
		{/if}
	</div>
	<div class="about-brand">
		<div class="about-logo">
			<LychiIcon size={56} />
		</div>
		<span class="about-name">Lychi</span>
	</div>

	<p class="about-desc">A fast, local-first command surface. Your data stays on your device. AI is optional, never required. Built for speed, privacy, and security.</p>

	<div class="about-links">
		<div class="about-link-row">
			<span class="about-link-label">Website</span>
			<span class="about-link-value">lychi.app</span>
		</div>
		<div class="about-link-row">
			<span class="about-link-label">Support</span>
			<span class="about-link-value">support@lychi.app</span>
		</div>
		<div class="about-link-row">
			<span class="about-link-label">Logs</span>
			<span class="about-link-stack">
				<button class="about-update-btn" onclick={() => openPath("~/.local/share/lychi/logs")}>
					Open folder
				</button>
				<span class="about-link-note">kept 7 days · typed commands never recorded</span>
			</span>
		</div>
		<div class="about-link-row">
			<span class="about-link-label">Features</span>
			<span class="about-link-value">feat@lychi.app</span>
		</div>
	</div>

	<div class="about-credits">
		<span class="about-credits-title">Credits</span>
		<div class="about-links">
			<div class="about-link-row">
				<span class="about-link-label">Weather data</span>
				<span class="about-link-value">MET Norway (CC BY 4.0)</span>
			</div>
			<div class="about-link-row">
				<span class="about-link-label">Geocoding</span>
				<span class="about-link-value">OpenStreetMap contributors (ODbL)</span>
			</div>
			<div class="about-link-row">
				<span class="about-link-label">Geolocation</span>
				<span class="about-link-value">ipwho.is</span>
			</div>
			<div class="about-link-row">
				<span class="about-link-label">Icons</span>
				<span class="about-link-value">Lucide (ISC)</span>
			</div>
		</div>
	</div>

	<p class="about-copy">&copy; {new Date().getFullYear()} Lychi. All rights reserved.</p>
</div>

<style>
	.about {
		position: relative;
		display: flex;
		flex-direction: column;
		gap: 8px;
		padding: 4px 0;
	}

	.about-brand {
		display: flex;
		flex-direction: row;
		align-items: center;
		justify-content: center;
		gap: 6px;
		padding: 12px 0;
	}

	.about-version-row {
		position: absolute;
		top: 8px;
		right: 8px;
		display: flex;
		align-items: center;
		gap: 6px;
	}

	.about-version-pill {
		font-size: 10px;
		color: var(--fg-muted);
		background: var(--border);
		padding: 2px 8px;
		border-radius: 10px;
	}

	.about-update-btn {
		font-size: 10px;
		color: var(--fg-muted);
		background: transparent;
		border: 1px solid var(--border);
		padding: 2px 8px;
		border-radius: 10px;
		cursor: pointer;
	}

	.about-update-btn:hover {
		color: var(--fg);
	}

	.about-update-ok {
		font-size: 10px;
		color: var(--success);
	}

	.about-update-found {
		font-size: 10px;
		color: var(--accent);
	}

	.about-update-hint {
		font-size: 10px;
		color: var(--fg-muted);
		max-width: 220px;
	}

	.about-logo {
		color: var(--fg);
		opacity: 0.9;
	}

	.about-name {
		font-size: 30px;
		font-weight: 300;
		letter-spacing: 0.1em;
		color: var(--fg);
		opacity: 0.9;
		font-family: var(--font-brand);
		text-transform: lowercase;
	}

	:global([data-theme="light"]) .about-logo,
	:global([data-theme="light"]) .about-name {
		opacity: 0.9;
	}

	.about-desc {
		font-size: 12px;
		color: var(--fg-muted);
		line-height: 1.5;
		margin: 0;
		text-align: center;
	}

	.about-links {
		display: flex;
		flex-direction: column;
		gap: 4px;
		margin-top: 4px;
	}

	.about-link-row {
		display: flex;
		align-items: baseline;
		justify-content: space-between;
		gap: 12px;
		padding: 5px 8px;
		background: var(--bg-secondary);
		border-radius: 4px;
	}

	.about-link-stack {
		display: flex;
		flex-direction: column;
		align-items: flex-end;
		gap: 1px;
	}

	.about-link-note {
		font-size: 10px;
		color: var(--fg-muted);
	}

	.about-link-label {
		font-size: 12px;
		color: var(--fg-muted);
	}

	.about-link-value {
		font-size: 12px;
		color: var(--fg);
	}

	.about-credits {
		display: flex;
		flex-direction: column;
		gap: 8px;
		padding-top: 4px;
		border-top: 1px solid var(--border);
	}

	.about-credits-title {
		font-size: 12px;
		font-weight: 600;
		color: var(--fg-muted);
		text-transform: uppercase;
		letter-spacing: 0.5px;
	}

	.about-copy {
		font-size: 11px;
		color: var(--fg-muted);
		margin: 4px 0 0 0;
		opacity: 0.6;
	}
</style>

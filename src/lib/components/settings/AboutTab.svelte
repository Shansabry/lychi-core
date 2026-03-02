<script lang="ts">
import LychiIcon from "../LychiIcon.svelte";

let { appVersion }: { appVersion: string } = $props();
</script>

<div class="about">
	<span class="about-version-pill">v{appVersion}</span>
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
				<span class="about-link-value">freeipapi.com</span>
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

	.about-version-pill {
		position: absolute;
		top: 8px;
		right: 8px;
		font-size: 10px;
		color: var(--fg-muted);
		background: var(--border);
		padding: 2px 8px;
		border-radius: 10px;
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
		align-items: center;
		justify-content: space-between;
		padding: 5px 8px;
		background: var(--bg-secondary);
		border-radius: 4px;
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

<script lang="ts">
let guideTab: "commands" | "triggers" = $state("commands");
</script>

<div class="guide" role="region" aria-label="Guide">
	<div class="guide-tab-bar">
		<button
			class="guide-tab"
			class:active={guideTab === "commands"}
			onmousedown={(e) => e.preventDefault()}
			onclick={() => { guideTab = "commands"; }}
			tabindex={-1}
		>Commands</button>
		<button
			class="guide-tab"
			class:active={guideTab === "triggers"}
			onmousedown={(e) => e.preventDefault()}
			onclick={() => { guideTab = "triggers"; }}
			tabindex={-1}
		>Triggers</button>
	</div>

	{#if guideTab === "commands"}
		<div class="guide-table">
			<div class="guide-row">
				<code>open &lt;app&gt;</code>
				<span>Launch an app</span>
			</div>
			<div class="guide-row">
				<code>web &lt;query&gt;</code>
				<span>Search the web</span>
			</div>
			<div class="guide-row">
				<code>yt &lt;query&gt;</code>
				<span>Search YouTube</span>
			</div>
			<div class="guide-row">
				<code>run &lt;cmd&gt;</code>
				<span>Run a shell command</span>
			</div>
			<div class="guide-row">
				<code>calc &lt;expr&gt;</code>
				<span>Evaluate math</span>
			</div>
			<div class="guide-row">
				<code>file &lt;path&gt;</code>
				<span>Open file or folder</span>
			</div>
			<div class="guide-row">
				<code>project &lt;name&gt;</code>
				<span>Open project in editor</span>
			</div>
			<div class="guide-row">
				<code>spotify &lt;action&gt;</code>
				<span>Control Spotify</span>
			</div>
			<div class="guide-row">
				<code>media &lt;action&gt;</code>
				<span>Control any media player</span>
			</div>
			<div class="guide-row">
				<code>note &lt;text&gt;</code>
				<span>Save a quick note</span>
			</div>
			<div class="guide-row">
				<code>todo &lt;action&gt;</code>
				<span>Manage todo list</span>
			</div>
			<div class="guide-row">
				<code>system &lt;action&gt;</code>
				<span>shutdown / reboot / lock / suspend</span>
			</div>
		</div>
	{:else}
		<div class="guide-table">
			<div class="guide-row">
				<code>=2+2</code>
				<span>Calculator</span>
			</div>
			<div class="guide-row">
				<code>&gt;ls -la</code>
				<span>Shell command</span>
			</div>
			<div class="guide-row">
				<code>~/Downloads</code>
				<span>Open path</span>
			</div>
			<div class="guide-row">
				<code>github.com</code>
				<span>Open URL</span>
			</div>
		</div>
	{/if}
</div>

<style>
	.guide {
		display: flex;
		flex-direction: column;
		padding: 2px 0;
		overflow-y: auto;
		max-height: calc(65vh - 40px);
	}

	.guide-tab-bar {
		display: flex;
		border-bottom: 1px solid var(--border);
		margin-bottom: 8px;
	}

	.guide-tab {
		font-family: var(--font-mono);
		font-size: 11px;
		text-transform: uppercase;
		letter-spacing: 0.5px;
		color: var(--fg-muted);
		background: none;
		border: none;
		border-bottom: 2px solid transparent;
		padding: 6px 10px;
		cursor: pointer;
		transition: color 100ms ease, border-color 100ms ease;
	}

	.guide-tab:hover {
		color: var(--fg);
	}

	.guide-tab.active {
		color: var(--fg);
		border-bottom-color: var(--accent);
	}

	.guide-table {
		display: flex;
		flex-direction: column;
		gap: 1px;
	}

	.guide-row {
		display: flex;
		align-items: center;
		gap: 12px;
		padding: 3px 0;
	}

	.guide-row code {
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--fg);
		background: var(--bg-secondary);
		padding: 2px 6px;
		border-radius: 3px;
		border: 1px solid var(--border);
		white-space: nowrap;
		min-width: 100px;
		text-align: center;
	}

	.guide-row span {
		font-size: 11px;
		color: var(--fg-muted);
	}
</style>

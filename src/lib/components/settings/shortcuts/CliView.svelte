<script lang="ts">
// Lychi's complete CLI surface. These drive a RUNNING instance over its IPC
// socket, so they work from a terminal, a script, or a key bound in the
// desktop's keyboard settings.
//
// Most act on whatever is on screen right now, which is why binding them to a
// key is the common use — but they are CLI commands first, and naming the tab
// after only one of their uses is what made the old copy misleading.
const COMMANDS = [
	{
		label: "Ask AI about selection",
		command: "lychi --ai",
		hint: "Sends whatever text you have highlighted.",
	},
	{
		label: "Run an AI command on it",
		command: "lychi --ai summarize",
		hint: "Any AI command keyword works in place of “summarize”.",
	},
	{
		label: "Capture a region",
		command: "lychi --screenshot area",
		hint: "Also accepts “window” or “screen”.",
	},
	{
		label: "Show or hide Lychi",
		command: "lychi --toggle",
		hint: "Only needed as a manual fallback — see below.",
	},
];
</script>

<div class="section-label first">CLI commands</div>
<div class="field-hint">
	Run these from a terminal or a script, or bind one to a key in your desktop's
	keyboard settings to use it without opening Lychi first. They act on whatever
	is on screen at the time.
</div>

<div class="cmd-list">
	{#each COMMANDS as item (item.command)}
		<div class="cmd-row">
			<div class="cmd-text">
				<span class="cmd-label">{item.label}</span>
				<span class="cmd-hint">{item.hint}</span>
			</div>
			<code class="cli-cmd">{item.command}</code>
		</div>
	{/each}
</div>

<div class="footnote">
	The summon hotkey is different: Lychi registers it with your desktop directly
	and it survives restarts, so it needs no manual binding. Bind
	<code>lychi --toggle</code> only if your compositor has no GlobalShortcuts
	portal — Sway and other wlroots compositors, or GNOME before 48.
</div>

<style>
	@import "./rows.css";

	.cmd-list {
		display: flex;
		flex-direction: column;
		gap: 2px;
	}

	/* Two columns that each get room: the label/hint stack can wrap freely while
	   the command stays on one line. The old single-line layout squeezed the
	   labels into two ragged lines to make space for the command. */
	.cmd-row {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: 16px;
		padding: 8px 0;
	}

	.cmd-row + .cmd-row {
		border-top: 1px solid color-mix(in srgb, var(--border) 45%, transparent);
	}

	.cmd-text {
		display: flex;
		flex-direction: column;
		gap: 2px;
		min-width: 0;
	}

	.cmd-label {
		color: var(--fg);
		font-size: 12px;
	}

	.cmd-hint {
		color: var(--fg-muted);
		font-size: 10.5px;
		opacity: 0.75;
	}

	/* Sits below the list, visually quieter than a hint above it: it qualifies
	   the last row rather than introducing the section. */
	.footnote {
		font-size: 10.5px;
		color: var(--fg-muted);
		opacity: 0.75;
		line-height: 1.55;
		padding: 12px 0 0;
		margin-top: 10px;
		border-top: 1px solid color-mix(in srgb, var(--border) 45%, transparent);
	}

	.footnote code {
		font-family: var(--font-mono);
		background: var(--bg-secondary);
		padding: 1px 4px;
		border-radius: 3px;
		user-select: all;
	}

	/* Selectable in one click — the point of this view is copying the command
	   into the desktop's own settings dialog. */
	.cli-cmd {
		padding: 4px 8px;
		background: var(--bg-secondary);
		border: 1px solid var(--border);
		border-radius: 4px;
		color: var(--fg);
		font-family: var(--font-mono);
		font-size: 11px;
		white-space: nowrap;
		flex-shrink: 0;
		user-select: all;
	}
</style>

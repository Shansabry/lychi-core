<script lang="ts">
import AnsiToHtml from "ansi-to-html";
import type { CommandResult } from "$lib/ipc";

let { result, command = "" }: { result: CommandResult; command?: string } = $props();

const darkColors = [
	"#5c6370",
	"#e06c75",
	"#98c379",
	"#e5c07b",
	"#61afef",
	"#c678dd",
	"#56b6c2",
	"#dcdfe4",
	"#747d8c",
	"#f44747",
	"#a8d89a",
	"#f0d98c",
	"#79c0ff",
	"#d8a0e8",
	"#73d0d8",
	"#ffffff",
];

const lightColors = [
	"#383a42",
	"#e45649",
	"#50a14f",
	"#c18401",
	"#4078f2",
	"#a626a4",
	"#0184bc",
	"#1a1a1a",
	"#696c77",
	"#ca1243",
	"#61a551",
	"#d4a017",
	"#5590f2",
	"#b751b6",
	"#1da0c4",
	"#333333",
];

let theme = $state(document.documentElement.dataset.theme ?? "dark");

// Watch for theme changes via MutationObserver
$effect(() => {
	const observer = new MutationObserver(() => {
		theme = document.documentElement.dataset.theme ?? "dark";
	});
	observer.observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });
	return () => observer.disconnect();
});

let converter = $derived(
	new AnsiToHtml({
		fg: theme === "light" ? "#1a1a1a" : "#e0e0e0",
		bg: "transparent",
		escapeXML: true,
		colors: theme === "light" ? lightColors : darkColors,
	}),
);

let outputHtml = $derived(result.output ? converter.toHtml(result.output) : "");
let errorHtml = $derived(result.error ? converter.toHtml(result.error) : "");
</script>

{#if result.output || result.error}
	<div class="result-panel">
		{#if command}
			<div class="command-header">
				<span class="prompt">$</span> {command}
			</div>
		{/if}
		{#if result.output}
			<pre class="output">{@html outputHtml}</pre>
		{/if}
		{#if result.error}
			<pre class="error">{@html errorHtml}</pre>
		{/if}
	</div>
{/if}

<style>
	.result-panel {
		padding: 12px 20px;
		max-height: 300px;
		overflow-y: auto;
		background: var(--bg-secondary);
	}

	.command-header {
		font-family: var(--font-mono);
		font-size: 12px;
		color: var(--fg-muted);
		padding-bottom: 6px;
		margin-bottom: 6px;
		border-bottom: 1px solid var(--border);
	}

	.prompt {
		color: var(--accent);
		margin-right: 6px;
	}

	pre {
		font-family: var(--font-mono);
		font-size: 13px;
		white-space: pre-wrap;
		word-break: break-word;
		line-height: 1.5;
	}

	.output {
		color: var(--fg);
	}

	.error {
		color: var(--error);
	}
</style>

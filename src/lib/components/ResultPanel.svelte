<script lang="ts">
import AnsiToHtml from "ansi-to-html";
import type { CommandResult } from "$lib/ipc";

let {
	result,
	command = "",
	onconfirm,
	ondismiss,
}: {
	result: CommandResult;
	command?: string;
	onconfirm?: () => void;
	ondismiss?: () => void;
} = $props();

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

let isHighRisk = $derived(result.risk_level === "high");

function autofocus(node: HTMLElement) {
	requestAnimationFrame(() => node.focus());
}

function handleKeydown(e: KeyboardEvent) {
	if (e.key === "Enter" && onconfirm) {
		e.preventDefault();
		e.stopPropagation();
		onconfirm();
	} else if (e.key === "Escape" && ondismiss) {
		e.preventDefault();
		e.stopPropagation();
		ondismiss();
	}
}
</script>

{#if result.needs_confirmation}
	<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
	<div class="confirm-panel" role="alertdialog" aria-label="Confirm command execution" onkeydown={handleKeydown} tabindex="-1"
		use:autofocus>
		<div class="confirm-header" class:high={isHighRisk}>
			<span class="confirm-icon">⚠</span>
			<span>Confirmation required</span>
		</div>
		{#if command}
			<div class="confirm-command">
				<span class="prompt">$</span> {command}
			</div>
		{/if}
		<div class="confirm-reason">{result.needs_confirmation}</div>
		{#if result.risk_level}
			<div class="confirm-risk" class:high={isHighRisk}>
				Risk: {result.risk_level}
			</div>
		{/if}
		<div class="confirm-actions">
			<button class="btn btn-cancel" onmousedown={(e) => e.preventDefault()} onclick={ondismiss}>
				Cancel <span class="kbd">Esc</span>
			</button>
			<button class="btn btn-confirm" class:high={isHighRisk} onmousedown={(e) => e.preventDefault()} onclick={onconfirm}>
				Confirm <span class="kbd">Enter</span>
			</button>
		</div>
	</div>
{:else if result.output || result.error}
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

	/* Confirmation panel */
	.confirm-panel {
		padding: 12px 20px;
		max-height: 300px;
		overflow-y: auto;
		background: var(--bg-secondary);
		font-family: var(--font-mono);
		font-size: 13px;
		color: var(--fg);
		outline: none;
	}

	.confirm-header {
		display: flex;
		align-items: center;
		gap: 6px;
		font-size: 12px;
		color: var(--fg-muted);
		margin-bottom: 8px;
	}

	.confirm-header.high {
		color: #ffaa00;
	}

	.confirm-icon {
		font-size: 13px;
	}

	.confirm-command {
		font-family: var(--font-mono);
		font-size: 13px;
		color: var(--accent);
		padding: 6px 8px;
		border-radius: 4px;
		background: var(--bg);
		border: 1px solid var(--border);
		margin-bottom: 8px;
	}

	.confirm-reason {
		font-size: 12px;
		color: var(--fg-muted);
		margin-bottom: 6px;
	}

	.confirm-risk {
		font-size: 11px;
		color: var(--fg-muted);
		margin-bottom: 8px;
	}

	.confirm-risk.high {
		color: #ffaa00;
	}

	.confirm-actions {
		display: flex;
		align-items: center;
		justify-content: flex-end;
		gap: 8px;
		padding-top: 10px;
		margin-top: 6px;
		border-top: 1px solid var(--border);
	}

	.btn {
		font-family: var(--font-mono);
		font-size: 11px;
		padding: 4px 12px;
		border-radius: 4px;
		border: 1px solid var(--border);
		cursor: pointer;
		display: flex;
		align-items: center;
		gap: 6px;
		transition: background 100ms ease;
	}

	.btn-cancel {
		background: transparent;
		color: var(--fg-muted);
	}

	.btn-cancel:hover {
		background: var(--bg);
	}

	.btn-confirm {
		background: var(--accent);
		color: var(--bg);
		border-color: var(--accent);
	}

	.btn-confirm:hover {
		opacity: 0.85;
	}

	.btn-confirm.high {
		background: #ffaa00;
		border-color: #ffaa00;
		color: #1a1a1a;
	}

	.kbd {
		font-size: 9px;
		opacity: 0.6;
	}
</style>

<script lang="ts">
import AnsiToHtml from "ansi-to-html";
import type { CommandResult } from "$lib/ipc";
import { getComboString, matchesAction } from "$lib/keybindings";
import { renderMarkdown } from "$lib/markdown";
import { confirmIntent } from "$lib/modalKeys";
import { resolveOutput } from "$lib/output";
import { sanitizeSvg, sanitizeTerminal } from "$lib/sanitize";
import RowsView from "./RowsView.svelte";
import WeatherCard from "./WeatherCard.svelte";

let {
	result,
	command = "",
	onconfirm,
	ondismiss,
	onopenurl,
	onopenfile,
	onrowaction,
}: {
	result: CommandResult;
	command?: string;
	onconfirm?: () => void;
	ondismiss?: () => void;
	onopenurl?: () => void;
	onopenfile?: (path: string) => void;
	/** A row action was chosen; the host resolves it through the executor. */
	onrowaction?: (handler: string, id: string, target: string) => void;
} = $props();

let hasInlineUrl = $derived(!!result.output && !!result.open_url);

// Which renderer to use is decided ONCE, in $lib/output, not by matching
// `output_type` here. This component used to own that mapping in an `{#if}`
// chain while `+page.svelte` separately matched the same field to decide
// whether a copy shortcut applied — two places deriving related verdicts from
// one field, which is how they drift. `resolved.actions` is the same decision
// expressed as capabilities, so the ⌘K panel can be adaptive without any
// surface knowing the type system.
let resolved = $derived(resolveOutput(result));
let isTerminal = $derived(resolved.renderer === "terminal");

// QR "copy image" — rasterize the inline SVG to a PNG and put it on the clipboard.
let svgWrap: HTMLDivElement | undefined = $state();
let qrCopied = $state(false);
// Exposed so a global keyboard shortcut (Ctrl+Shift+C) can copy the QR too.
export function copyQr() {
	if (isSvg) copyQrPng();
}
async function copyQrPng() {
	const svg = svgWrap?.querySelector("svg");
	if (!svg) return;
	try {
		// Serialize the SVG, draw it onto a canvas at a crisp fixed size, export PNG.
		const size = 512; // high-res so the pasted image scans well
		const xml = new XMLSerializer().serializeToString(svg);
		const svgUrl = `data:image/svg+xml;charset=utf-8,${encodeURIComponent(xml)}`;
		const img = new Image();
		await new Promise<void>((resolve, reject) => {
			img.onload = () => resolve();
			img.onerror = () => reject(new Error("svg load failed"));
			img.src = svgUrl;
		});
		const canvas = document.createElement("canvas");
		canvas.width = size;
		canvas.height = size;
		const ctx = canvas.getContext("2d");
		if (!ctx) return;
		ctx.fillStyle = "#ffffff";
		ctx.fillRect(0, 0, size, size);
		ctx.drawImage(img, 0, 0, size, size);
		const blob: Blob | null = await new Promise((resolve) =>
			canvas.toBlob((b) => resolve(b), "image/png"),
		);
		if (!blob) return;
		await navigator.clipboard.write([new ClipboardItem({ "image/png": blob })]);
		qrCopied = true;
		setTimeout(() => {
			qrCopied = false;
		}, 1500);
	} catch (err) {
		console.error("[qr] copy failed:", err);
	}
}

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

// Only convert ANSI for terminal output and errors
let outputHtml = $derived(
	isTerminal && result.output ? sanitizeTerminal(converter.toHtml(result.output)) : "",
);
let errorHtml = $derived(result.error ? sanitizeTerminal(converter.toHtml(result.error)) : "");

// SVG output (e.g. a QR code) — embedded inline, crisp and scannable. Sanitized
// to strip any scripts/handlers while keeping the vector shapes.
// Markdown reuses the chat's renderer rather than a second one — same `marked`
// + highlight.js + DOMPurify path, so a code block looks identical whether it
// came from the AI panel or a command result. Its own header anticipated this
// ("and any future markdown surface"); this is that surface.
let isMarkdown = $derived(resolved.renderer === "markdown");
let markdownHtml = $derived(isMarkdown && result.output ? renderMarkdown(result.output) : "");

let isSvg = $derived(resolved.renderer === "svg");
let svgHtml = $derived(isSvg && result.output ? sanitizeSvg(result.output) : "");

// --- Clickable filenames in ls output ---

/** Extract target directory from an ls-like command. Returns null for non-ls commands. */
function extractLsDirectory(cmd: string): string | null {
	const m = cmd.trim().match(/^ls\s*((?:-\w+\s+)*)(.+)?$/);
	if (!m) return null;
	const dir = m[2]?.trim();
	return dir || "~";
}

/** Escape a string for use in an HTML attribute. */
function escapeAttr(s: string): string {
	return s
		.replace(/&/g, "&amp;")
		.replace(/"/g, "&quot;")
		.replace(/</g, "&lt;")
		.replace(/>/g, "&gt;");
}

// Permission pattern for ls -l lines: drwxr-xr-x, -rw-r--r--, lrwxrwxrwx, etc.
const PERM_RE = /^[d\-lbcps][rwxsStT-]{9}/;
// Match the time portion (HH:MM or year) to find where the filename starts
const LS_LONG_RE =
	/^[d\-lbcps][rwxsStT-]{9}[\s.+]+\S+\s+\S+\s+\S+\s+[\d,]+\s+\w+\s+\d+\s+[\d:]+\s+/;

/** Wrap filenames in ls output with clickable spans. Works on already-HTML-escaped text. */
function linkifyLsOutput(html: string, directory: string): string {
	return html
		.split("\n")
		.map((line) => {
			// Strip HTML tags to inspect the raw text content
			const raw = line.replace(/<[^>]*>/g, "").trim();
			if (!raw || raw.startsWith("total ")) return line;

			let filename: string;
			if (PERM_RE.test(raw)) {
				// Long format: extract filename after the date/time columns
				const match = raw.match(LS_LONG_RE);
				if (!match) return line;
				filename = raw.slice(match[0].length);
				// Handle symlinks: "name -> target"
				const arrow = filename.indexOf(" -> ");
				if (arrow !== -1) filename = filename.slice(0, arrow);
			} else {
				// Plain ls output: each line is a filename
				filename = raw;
			}

			if (!filename) return line;
			const filepath = directory.endsWith("/")
				? `${directory}${filename}`
				: `${directory}/${filename}`;
			return `<span class="clickable-file" data-filepath="${escapeAttr(filepath)}">${line}</span>`;
		})
		.join("\n");
}

let lsDirectory = $derived(
	isTerminal && result.executed_args ? extractLsDirectory(result.executed_args) : null,
);
let processedHtml = $derived(
	outputHtml && lsDirectory
		? sanitizeTerminal(linkifyLsOutput(outputHtml, lsDirectory))
		: outputHtml,
);

let isHighRisk = $derived(result.risk_level === "high");

function handleTerminalClick(e: MouseEvent | KeyboardEvent) {
	const target = (e.target as HTMLElement).closest(".clickable-file");
	if (!target || !onopenfile) return;
	const filepath = target.getAttribute("data-filepath");
	if (filepath) {
		e.preventDefault();
		onopenfile(filepath);
	}
}

function autofocus(node: HTMLElement, shouldFocus: boolean = true) {
	if (shouldFocus) {
		requestAnimationFrame(() => node.focus());
	}
}

function handleKeydown(e: KeyboardEvent) {
	// The approve/reject decision lives in $lib/modalKeys — one pure function
	// shared with the AI prompt, testable without a component harness. This
	// panel used to hardcode `e.key === "Enter"` and ignore the binding table,
	// so the two confirmation surfaces disagreed about their own approve key
	// (docs/issues.md I-016).
	const intent = confirmIntent(e);
	if (intent === "approve" && onconfirm) {
		e.preventDefault();
		e.stopPropagation();
		onconfirm();
	} else if (intent === "reject" && ondismiss) {
		e.preventDefault();
		e.stopPropagation();
		ondismiss();
	} else if (matchesAction(e, "open_inline_url") && onopenurl && hasInlineUrl) {
		e.preventDefault();
		e.stopPropagation();
		onopenurl();
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
			<!-- One advertised key, two working ones. Enter is the primary (the
			     panel autofocuses, and it approves even if approve_action is
			     rebound). The approve_action combo ALSO approves - it exists for
			     when focus is back in the command input, where plain Enter would
			     submit the input instead (I-016) - but stacking both kbds on the
			     button read as a rendering bug, and the sibling AI-approval
			     surface shows a single kbd. Keep the keys, advertise the primary. -->
			<button class="btn btn-confirm" class:high={isHighRisk} onmousedown={(e) => e.preventDefault()} onclick={onconfirm}
				title={`Confirm (Enter, or ${getComboString("approve_action")} while typing)`}>
				Confirm <span class="kbd">Enter</span>
			</button>
		</div>
	</div>
{:else if result.output || result.error || resolved.renderer === "rows"}
	<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
	<div class="result-panel" class:has-url={hasInlineUrl} onkeydown={hasInlineUrl ? handleKeydown : undefined}
		tabindex={hasInlineUrl ? -1 : undefined} role={hasInlineUrl ? "region" : undefined}
		use:autofocus={hasInlineUrl}>
		{#if command && isTerminal}
			<div class="command-header">
				<span class="prompt">$</span> {command}
			</div>
		{/if}
		{#if result.output || resolved.renderer === "rows"}
			{#if isTerminal}
				<pre class="output terminal" role={lsDirectory ? "button" : undefined} tabindex={lsDirectory ? 0 : undefined} onclick={lsDirectory ? handleTerminalClick : undefined} onkeydown={lsDirectory ? (e) => { if (e.key === 'Enter') handleTerminalClick(e); } : undefined}>{@html processedHtml}</pre>
			{:else if isSvg}
				<!-- Inline SVG (QR code) — crisp, scannable, compact. -->
				<div class="output svg-output">
					<div class="svg-wrap" bind:this={svgWrap}>{@html svgHtml}</div>
					<button class="btn btn-copy-qr" onmousedown={(e) => e.preventDefault()} onclick={copyQrPng}>
						{qrCopied ? "Copied" : "Copy image"}
						{#if !qrCopied}<span class="kbd">{getComboString("copy_path")}</span>{/if}
					</button>
				</div>
			{:else if isMarkdown}
				<!-- Sanitized upstream by renderMarkdown (DOMPurify); the content is
				     model or third-party output and is never trusted raw. -->
				<div class="output markdown md-body">{@html markdownHtml}</div>
			{:else if resolved.renderer === "text"}
				<div class="output text">{result.output}</div>
			{:else if resolved.renderer === "rows"}
				<RowsView sections={result.sections ?? []} onaction={onrowaction} />
			{:else if resolved.renderer === "weather"}
				<!-- The legacy structured-output path: JSON smuggled through the
				     string field and parsed back out here. It is the reason
				     `Output::Rows` crosses IPC typed instead — this pattern works
				     exactly once and cannot be reused without copying the hack.
				     Owed a migration onto a typed variant; guarded meanwhile so a
				     missing/!JSON payload cannot throw during render. -->
				<WeatherCard data={JSON.parse(result.output ?? "{}")} />
			{:else}
				<div class="output status">{result.output}</div>
			{/if}
		{/if}
		{#if result.error}
			<pre class="output-error">{@html errorHtml}</pre>
		{/if}
		{#if hasInlineUrl}
			<div class="inline-url-actions">
				<button class="btn btn-browser" onmousedown={(e) => e.preventDefault()} onclick={onopenurl}>
					Browse <span class="kbd">{getComboString("open_inline_url")}</span>
				</button>
			</div>
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

	/* Terminal output — fixed-width with ANSI colors. Uses --font-output, NOT
	   --font-mono: the font picker retargets --font-mono for the whole UI, but
	   command output must stay aligned whatever the user picks. */
	pre {
		font-family: var(--font-output);
		font-size: 13px;
		white-space: pre-wrap;
		word-break: break-word;
		line-height: 1.5;
	}

	.output.terminal {
		color: var(--fg);
	}

	/* Inline SVG output (QR code): compact, centered, on a white plate so it
	   scans in dark mode too. Fixed modest size — SVG scales crisply. */
	.output.svg-output {
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 8px;
		padding: 12px;
	}

	.output.svg-output :global(svg) {
		width: 180px;
		height: 180px;
		background: var(--plate);
		border-radius: 6px;
		padding: 8px;
		box-sizing: border-box;
	}

	.btn-copy-qr {
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--fg-muted);
		background: none;
		border: 1px solid var(--border);
		border-radius: 6px;
		padding: 3px 12px;
		cursor: pointer;
		transition: color 100ms ease, border-color 100ms ease;
	}

	.btn-copy-qr:hover {
		color: var(--fg);
		border-color: var(--fg-muted);
	}

	.output.terminal :global(.clickable-file) {
		cursor: pointer;
		border-radius: 2px;
		display: inline-block;
		width: 100%;
	}

	.output.terminal :global(.clickable-file:hover) {
		background: var(--bg);
		text-decoration: underline;
	}

	/* Text output — clean readable sans-serif (AI answers, notes) */
	.output.text {
		font-family: var(--font-sans, system-ui, -apple-system, sans-serif);
		font-size: 13px;
		line-height: 1.6;
		color: var(--fg);
		white-space: pre-wrap;
		word-break: break-word;
	}

	/* Status output — compact muted (short messages) */
	.output.status {
		font-family: var(--font-mono);
		font-size: 12px;
		color: var(--fg-muted);
	}

	.output-error {
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
		color: var(--warning);
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
		color: var(--warning);
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
		background: var(--warning);
		border-color: var(--warning);
		color: var(--plate-fg);
	}

	.kbd {
		font-size: 9px;
		opacity: 0.6;
	}

	/* Inline URL action bar (e.g. "ask" handler results) */
	.result-panel.has-url {
		outline: none;
	}

	.inline-url-actions {
		display: flex;
		justify-content: flex-end;
		padding-top: 8px;
		margin-top: 8px;
		border-top: 1px solid var(--border);
	}

	.btn-browser {
		font-family: var(--font-mono);
		font-size: 11px;
		padding: 5px 14px;
		border-radius: 4px;
		border: 1px solid var(--accent);
		cursor: pointer;
		display: flex;
		align-items: center;
		gap: 6px;
		transition: all 100ms ease;
		background: var(--accent);
		color: var(--bg);
	}

	.btn-browser:hover {
		opacity: 0.85;
	}
</style>

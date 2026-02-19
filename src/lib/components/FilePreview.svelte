<script lang="ts">
import { marked } from "marked";
import { LoaderCircle, FileText, Image, FileQuestion } from "lucide-svelte";
import { getFilePreview, type FilePreviewData } from "$lib/ipc";

let {
	filePath,
	visible,
}: {
	filePath: string;
	visible: boolean;
} = $props();

let preview: FilePreviewData | null = $state(null);
let loading = $state(false);
let error: string | null = $state(null);
let debounceTimer: ReturnType<typeof setTimeout> | undefined;

// Track the path we last loaded to avoid re-fetching
let loadedPath = $state("");

$effect(() => {
	if (!visible || !filePath) {
		preview = null;
		loadedPath = "";
		return;
	}

	if (filePath === loadedPath) return;

	clearTimeout(debounceTimer);
	loading = true;
	error = null;

	const targetPath = filePath;
	debounceTimer = setTimeout(async () => {
		try {
			const result = await getFilePreview(targetPath);
			// Only apply if path hasn't changed during fetch
			if (targetPath === filePath) {
				preview = result;
				loadedPath = targetPath;
				error = null;
			}
		} catch (e) {
			if (targetPath === filePath) {
				error = String(e);
				preview = null;
			}
		} finally {
			if (targetPath === filePath) {
				loading = false;
			}
		}
	}, 150);
});

// Render markdown to HTML
let renderedMarkdown = $derived(
	preview?.kind === "Text" && preview.language === "markdown"
		? marked.parse(preview.content)
		: "",
);

// Extract filename from path
let fileName = $derived(() => {
	const last = filePath.lastIndexOf("/");
	return last === -1 ? filePath : filePath.slice(last + 1);
});

function formatBytes(bytes: number): string {
	if (bytes < 1024) return `${bytes} B`;
	if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
	return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}
</script>

{#if visible}
<div class="preview-panel">
	{#if loading && !preview}
		<div class="preview-loading">
			<LoaderCircle size={16} strokeWidth={1.5} class="spinner" />
			<span>Loading preview...</span>
		</div>
	{:else if error}
		<div class="preview-error">
			<span>Preview unavailable</span>
		</div>
	{:else if preview}
		{#if preview.kind === "Text"}
			<div class="preview-header">
				<FileText size={14} strokeWidth={1.5} />
				<span class="lang-badge">{preview.language}</span>
				{#if preview.truncated}
					<span class="truncated-badge">truncated</span>
				{/if}
			</div>
			{#if preview.language === "markdown" && renderedMarkdown}
				<div class="preview-content markdown">{@html renderedMarkdown}</div>
			{:else}
				<pre class="preview-content code">{preview.content}</pre>
			{/if}
		{:else if preview.kind === "Image"}
			<div class="preview-content image-container">
				<img src="data:{preview.mime};base64,{preview.base64}" alt={fileName()} />
			</div>
		{:else if preview.kind === "Unsupported"}
			<div class="preview-unsupported">
				<FileQuestion size={24} strokeWidth={1.5} />
				<span class="unsupported-type">{preview.mime}</span>
				<span class="unsupported-size">{formatBytes(preview.size_bytes)}</span>
			</div>
		{/if}
	{/if}
</div>
{/if}

<style>
	.preview-panel {
		position: absolute;
		left: calc(100% + 10px);
		top: 0;
		width: 380px;
		max-height: 60vh;
		background: var(--bg);
		border-radius: 12px;
		box-shadow: 0 8px 32px rgba(0, 0, 0, 0.5);
		overflow: hidden;
		display: flex;
		flex-direction: column;
		animation: preview-appear 120ms ease-out;
	}

	@keyframes preview-appear {
		from {
			opacity: 0;
			transform: translateX(-6px);
		}
		to {
			opacity: 1;
			transform: translateX(0);
		}
	}

	.preview-header {
		display: flex;
		align-items: center;
		gap: 8px;
		padding: 8px 14px;
		border-bottom: 1px solid var(--border);
		color: var(--fg-muted);
		flex-shrink: 0;
	}

	.lang-badge {
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--accent);
		background: var(--bg-secondary);
		padding: 1px 8px;
		border-radius: 8px;
	}

	.truncated-badge {
		font-family: var(--font-mono);
		font-size: 10px;
		color: var(--fg-muted);
		opacity: 0.5;
		margin-left: auto;
	}

	.preview-content {
		overflow-y: auto;
		flex: 1;
		min-height: 0;
	}

	.preview-content.code {
		font-family: var(--font-mono);
		font-size: 12px;
		line-height: 1.5;
		color: var(--fg);
		padding: 12px 14px;
		white-space: pre-wrap;
		word-break: break-word;
		margin: 0;
	}

	.preview-content.markdown {
		font-family: var(--font-sans, system-ui, -apple-system, sans-serif);
		font-size: 13px;
		line-height: 1.6;
		color: var(--fg);
		padding: 12px 14px;
		overflow-wrap: break-word;
	}

	/* Markdown element styles */
	.preview-content.markdown :global(h1),
	.preview-content.markdown :global(h2),
	.preview-content.markdown :global(h3),
	.preview-content.markdown :global(h4) {
		color: var(--fg);
		margin: 16px 0 8px;
		line-height: 1.3;
	}

	.preview-content.markdown :global(h1) { font-size: 18px; }
	.preview-content.markdown :global(h2) { font-size: 16px; }
	.preview-content.markdown :global(h3) { font-size: 14px; }

	.preview-content.markdown :global(p) {
		margin: 8px 0;
	}

	.preview-content.markdown :global(code) {
		font-family: var(--font-mono);
		font-size: 12px;
		background: var(--bg-secondary);
		padding: 1px 4px;
		border-radius: 3px;
	}

	.preview-content.markdown :global(pre) {
		background: var(--bg-secondary);
		padding: 10px 12px;
		border-radius: 6px;
		overflow-x: auto;
		margin: 8px 0;
	}

	.preview-content.markdown :global(pre code) {
		background: none;
		padding: 0;
	}

	.preview-content.markdown :global(ul),
	.preview-content.markdown :global(ol) {
		padding-left: 20px;
		margin: 8px 0;
	}

	.preview-content.markdown :global(blockquote) {
		border-left: 3px solid var(--accent);
		margin: 8px 0;
		padding: 4px 12px;
		color: var(--fg-muted);
	}

	.preview-content.markdown :global(a) {
		color: var(--accent);
		text-decoration: none;
	}

	.preview-content.markdown :global(hr) {
		border: none;
		border-top: 1px solid var(--border);
		margin: 12px 0;
	}

	.preview-content.markdown :global(img) {
		max-width: 100%;
		border-radius: 4px;
	}

	.preview-content.markdown :global(table) {
		border-collapse: collapse;
		width: 100%;
		margin: 8px 0;
	}

	.preview-content.markdown :global(th),
	.preview-content.markdown :global(td) {
		border: 1px solid var(--border);
		padding: 4px 8px;
		font-size: 12px;
	}

	.preview-content.markdown :global(th) {
		background: var(--bg-secondary);
	}

	.image-container {
		display: flex;
		align-items: center;
		justify-content: center;
		padding: 12px;
	}

	.image-container img {
		max-width: 100%;
		max-height: calc(60vh - 24px);
		object-fit: contain;
		border-radius: 4px;
	}

	.preview-loading {
		display: flex;
		align-items: center;
		justify-content: center;
		gap: 8px;
		padding: 24px;
		color: var(--fg-muted);
		font-family: var(--font-mono);
		font-size: 12px;
	}

	.preview-loading :global(.spinner) {
		animation: spin 700ms linear infinite;
	}

	@keyframes spin {
		from { transform: rotate(0deg); }
		to { transform: rotate(360deg); }
	}

	.preview-error {
		display: flex;
		align-items: center;
		justify-content: center;
		padding: 24px;
		color: var(--fg-muted);
		font-family: var(--font-mono);
		font-size: 12px;
		opacity: 0.6;
	}

	.preview-unsupported {
		display: flex;
		flex-direction: column;
		align-items: center;
		justify-content: center;
		gap: 8px;
		padding: 32px 24px;
		color: var(--fg-muted);
	}

	.unsupported-type {
		font-family: var(--font-mono);
		font-size: 12px;
		opacity: 0.6;
	}

	.unsupported-size {
		font-family: var(--font-mono);
		font-size: 11px;
		opacity: 0.4;
	}

	/* Shrink preview on medium landscape screens */
	@media (max-width: 1400px) and (min-width: 1101px) {
		.preview-panel {
			width: 280px;
		}
	}

	/* Narrow screens: reposition below the launcher instead of to the right */
	@media (max-width: 1100px) {
		.preview-panel {
			position: absolute;
			left: 0;
			top: 100%;
			margin-top: 8px;
			width: 100%;
			max-height: 20vh;
		}
	}

	/* Portrait orientation: always below, compact height since launcher uses most vertical space */
	@media (orientation: portrait) {
		.preview-panel {
			position: absolute;
			left: 0;
			top: 100%;
			margin-top: 8px;
			width: 100%;
			max-height: 18vh;
		}
	}

	/* Short viewports (e.g. landscape ultra-short or small screens) */
	@media (max-height: 600px) {
		.preview-panel {
			max-height: 25vh;
		}
	}
</style>

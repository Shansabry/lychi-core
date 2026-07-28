<script lang="ts">
import { File, FileQuestion, FileText, Folder, Image, LoaderCircle } from "lucide-svelte";
import { marked } from "marked";
import { type FilePreviewData, getFilePreview } from "$lib/ipc";
import { sanitizeMarkdown } from "$lib/sanitize";

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
let renderedMarkdown = $derived.by(() => {
	if (preview && preview.kind === "Text" && preview.language === "markdown") {
		return sanitizeMarkdown(marked.parse(preview.content) as string);
	}
	return "";
});

// Extract filename from path
let fileName = $derived(() => {
	const last = filePath.lastIndexOf("/");
	return last === -1 ? filePath : filePath.slice(last + 1);
});

function formatBytes(bytes: number): string {
	if (bytes < 1024) return `${bytes} B`;
	if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
	if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
	return `${(bytes / (1024 * 1024 * 1024)).toFixed(1)} GB`;
}

function formatRelativeTime(epoch: number): string {
	if (!epoch) return "";
	const now = Math.floor(Date.now() / 1000);
	const diff = now - epoch;
	if (diff < 60) return "just now";
	if (diff < 3600) return `${Math.floor(diff / 60)}m ago`;
	if (diff < 86400) return `${Math.floor(diff / 3600)}h ago`;
	if (diff < 604800) return `${Math.floor(diff / 86400)}d ago`;
	const d = new Date(epoch * 1000);
	return d.toLocaleDateString(undefined, {
		month: "short",
		day: "numeric",
		year: d.getFullYear() !== new Date().getFullYear() ? "numeric" : undefined,
	});
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
		<div class="preview-meta">
			<span class="meta-name" title={preview.full_path}>{fileName()}</span>
			<div class="meta-details">
				{#if preview.kind === "Text"}
					<span class="meta-badge">{preview.language}</span>
				{:else if preview.kind === "Directory"}
					<span class="meta-badge">{preview.item_count} item{preview.item_count === 1 ? "" : "s"}</span>
				{:else if preview.kind === "Unsupported"}
					<span class="meta-badge">{preview.mime}</span>
				{/if}
				{#if preview.size_bytes > 0}
					<span class="meta-sep">&middot;</span>
					<span>{formatBytes(preview.size_bytes)}</span>
				{/if}
				{#if preview.modified_epoch}
					<span class="meta-sep">&middot;</span>
					<span>{formatRelativeTime(preview.modified_epoch)}</span>
				{/if}
				{#if preview.kind === "Text" && preview.truncated}
					<span class="meta-sep">&middot;</span>
					<span class="truncated-badge">truncated</span>
				{/if}
			</div>
		</div>
		{#if preview.kind === "Text"}
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
				<span class="unsupported-label">No preview available</span>
			</div>
		{:else if preview.kind === "Directory"}
			<div class="preview-content dir-listing">
				{#each preview.children as child}
					<div class="dir-child">
						{#if child.is_dir}
							<Folder size={13} strokeWidth={1.5} class="dir-child-icon-folder" />
						{:else}
							<File size={13} strokeWidth={1.5} class="dir-child-icon-file" />
						{/if}
						<span class="dir-child-name">{child.name}{child.is_dir ? "/" : ""}</span>
					</div>
				{/each}
				{#if preview.item_count > preview.children.length}
					<div class="dir-child dir-child-more">
						+{preview.item_count - preview.children.length} more
					</div>
				{/if}
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
		box-shadow: 0 8px 32px var(--shadow-overlay);
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

	.preview-meta {
		padding: 10px 14px 8px;
		border-bottom: 1px solid var(--border);
		flex-shrink: 0;
	}

	.meta-name {
		display: block;
		font-family: var(--font-mono);
		font-size: 12px;
		font-weight: 600;
		color: var(--fg);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
		margin-bottom: 4px;
	}

	.meta-details {
		display: flex;
		align-items: center;
		gap: 4px;
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--fg-muted);
	}

	.meta-badge {
		color: var(--accent);
		background: var(--bg-secondary);
		padding: 1px 8px;
		border-radius: 8px;
	}

	.meta-sep {
		opacity: 0.4;
	}

	.truncated-badge {
		opacity: 0.5;
	}

	.preview-content {
		overflow-y: auto;
		flex: 1;
		min-height: 0;
	}

	.preview-content.code {
		/* Source code: fixed-width regardless of the UI font choice. */
		font-family: var(--font-output);
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
		font-family: var(--font-output);
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

	.dir-listing {
		padding: 6px 0;
	}

	.dir-child {
		display: flex;
		align-items: center;
		gap: 8px;
		padding: 3px 14px;
		font-family: var(--font-mono);
		font-size: 12px;
		color: var(--fg);
	}

	.dir-child :global(.dir-child-icon-folder) {
		color: var(--accent);
		flex-shrink: 0;
	}

	.dir-child :global(.dir-child-icon-file) {
		color: var(--fg-muted);
		opacity: 0.5;
		flex-shrink: 0;
	}

	.dir-child-name {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.dir-child-more {
		color: var(--fg-muted);
		opacity: 0.5;
		font-size: 11px;
		padding-left: 35px;
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

	.unsupported-label {
		font-family: var(--font-mono);
		font-size: 12px;
		opacity: 0.5;
	}

	/* Narrow viewport (680px surface or portrait): stack below launcher instead of side-by-side */
	@media (max-width: 760px) {
		.preview-panel {
			position: static;
			width: 100%;
			max-height: 40vh;
			margin-top: 10px;
			animation-name: preview-appear-below;
		}

		.image-container img {
			max-height: calc(40vh - 24px);
		}
	}

	@keyframes preview-appear-below {
		from {
			opacity: 0;
			transform: translateY(-6px);
		}
		to {
			opacity: 1;
			transform: translateY(0);
		}
	}
</style>

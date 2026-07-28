<script lang="ts">
/**
 * The staged-attachment tray — a row of chips between the input and the
 * completions list, showing which files the next AI turn will be asked about.
 *
 * Rendering only. Every verdict on the chip (icon, whether it's usable, the
 * explanatory note) comes from the backend's `classify_files`; this component
 * never inspects a path or extension itself.
 *
 * Per the WebKit paint notes in the perf memory, the tray keeps its DOM node
 * alive and toggles `visibility` rather than mounting/unmounting on every
 * attach — the tray is on the hot typing path.
 */

import { FileText, Paperclip, TriangleAlert, X } from "lucide-svelte";
import { attachments } from "$lib/stores/attachments.svelte";

/** Human-readable size for the chip tooltip. */
function humanSize(bytes: number | null): string {
	if (bytes === null) return "";
	const units = ["B", "KB", "MB", "GB"];
	let n = bytes;
	let i = 0;
	while (n >= 1024 && i < units.length - 1) {
		n /= 1024;
		i++;
	}
	return `${i === 0 ? n : n.toFixed(1)} ${units[i]}`;
}
</script>

<div class="tray" class:visible={attachments.any} aria-label="Attached files">
	{#each attachments.items as att (att.path)}
		{@const broken = att.route === "unsupported"}
		<div
			class="chip"
			class:broken
			title={att.note ?? `${att.path}${att.size_bytes !== null ? ` · ${humanSize(att.size_bytes)}` : ""}`}
		>
			{#if att.thumbnail}
				<img class="thumb" src={att.thumbnail} alt="" />
			{:else if broken}
				<TriangleAlert class="chip-icon" size={13} strokeWidth={1.5} />
			{:else if att.kind === "doc" || att.kind === "text"}
				<FileText class="chip-icon" size={13} strokeWidth={1.5} />
			{:else}
				<Paperclip class="chip-icon" size={13} strokeWidth={1.5} />
			{/if}
			<span class="name">{att.name}</span>
			<button
				type="button"
				class="remove"
				aria-label={`Remove ${att.name}`}
				onclick={() => attachments.remove(att.path)}
			>
				<X size={11} strokeWidth={2} />
			</button>
		</div>
	{/each}
	{#if attachments.visionMismatch}
		<!-- Shown only when we have EVIDENCE this model rejects images (a prior
		     rejection, or provider metadata). An unknown model shows nothing —
		     it may well work, and nagging would train the user to ignore this. -->
		<span class="vision-warning">
			<TriangleAlert size={12} strokeWidth={1.75} />
			This model can't read images — it will answer from your text only.
		</span>
	{/if}
</div>

<style>
	.tray {
		display: flex;
		flex-wrap: wrap;
		gap: 6px;
		padding: 0 20px 10px;
		/* Kept in the layout and toggled by visibility — creating/destroying this
		   row on every attach costs a WebKit layout pass on the typing path. */
		visibility: hidden;
		max-height: 0;
		overflow: hidden;
		pointer-events: none;
		will-change: transform;
	}

	.tray.visible {
		visibility: visible;
		max-height: none;
		overflow: visible;
		pointer-events: auto;
	}

	.chip {
		display: flex;
		align-items: center;
		gap: 6px;
		max-width: 220px;
		padding: 3px 4px 3px 7px;
		background: var(--bg-secondary);
		border: 1px solid var(--border);
		border-radius: 6px;
		font-family: var(--font-sans);
		font-size: 11px;
		color: var(--fg);
		user-select: none;
	}

	.chip.broken {
		border-color: color-mix(in srgb, var(--error) 45%, var(--border));
		color: var(--fg-muted);
	}

	.chip :global(.chip-icon) {
		flex-shrink: 0;
		color: var(--fg-muted);
	}

	.chip.broken :global(.chip-icon) {
		color: var(--error);
	}

	.thumb {
		flex-shrink: 0;
		width: 18px;
		height: 18px;
		object-fit: cover;
		border-radius: 3px;
	}

	.name {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.remove {
		display: flex;
		align-items: center;
		flex-shrink: 0;
		padding: 2px;
		background: none;
		border: none;
		border-radius: 4px;
		color: var(--fg-muted);
		cursor: pointer;
		opacity: 0.6;
	}

	.remove:hover {
		opacity: 1;
		background: var(--overlay-wash);
	}

	/* A caution, not an error: the send still works, the model just won't see
	   the image. Full-width so it reads as a note about the tray, not a chip. */
	.vision-warning {
		display: flex;
		align-items: center;
		gap: 6px;
		flex-basis: 100%;
		font-family: var(--font-sans);
		font-size: 11px;
		color: var(--fg-muted);
	}

	.vision-warning :global(svg) {
		flex-shrink: 0;
		color: var(--cat-system);
	}
</style>

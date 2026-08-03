<script lang="ts">
/**
 * Renders a `Rows` result — the shared list renderer.
 *
 * ONE component for every list-shaped result. `services`, `packages`, `ssh`,
 * `symbol`, `clipboard`, windows, notes, snippets and timers are all "a list of
 * things, each with a state and some actions"; before this each produced its
 * own pre-formatted string, so a dozen handlers each invented their own column
 * alignment and none of them could be acted on.
 *
 * The row data arrives typed (specta-generated from the Rust `Row`), not as
 * JSON smuggled through a string field the way the weather card does it — which
 * is why that pattern was never reused and this one can be.
 */
import type { Row, Section } from "$lib/bindings";

let {
	sections,
	onaction,
}: {
	sections: Section[];
	/** Invoked with the producing handler + action id + target; the host resolves it backend-side. */
	onaction?: (handler: string, id: string, target: string) => void;
} = $props();

let isEmpty = $derived(sections.every((s) => s.rows.length === 0));

/**
 * Relative time, formatted HERE rather than by the handler.
 *
 * A handler computing "3 days ago" is the same mistake as a handler padding
 * columns: it bakes presentation into data, and every handler phrases it
 * slightly differently. The backend sends a unix timestamp; ages read
 * identically across timers, notes, clipboard and history because they all
 * pass through this one function.
 */
function relative(at: number): string {
	const secs = Math.max(0, Math.floor(Date.now() / 1000) - at);
	if (secs < 45) return "now";
	const mins = Math.round(secs / 60);
	if (mins < 60) return `${mins}m`;
	const hours = Math.round(mins / 60);
	if (hours < 24) return `${hours}h`;
	const days = Math.round(hours / 24);
	if (days < 30) return `${days}d`;
	const months = Math.round(days / 30);
	return months < 12 ? `${months}mo` : `${Math.round(months / 12)}y`;
}

/** The row's default action — the one Enter takes. */
function primaryAction(row: Row) {
	return row.actions[0];
}
</script>

{#snippet rowBody(row: Row)}
	<div class="row-main">
		<span class="row-title">{row.title}</span>
		{#if row.subtitle}<span class="row-subtitle">{row.subtitle}</span>{/if}
	</div>
	{#if row.badge}
		<span class="badge badge-{row.badge.tone}">{row.badge.text}</span>
	{/if}
	{#each row.accessories as acc, i (i)}
		<span class="accessory">
			{acc.kind === "relative" ? relative(acc.at) : acc.value}
		</span>
	{/each}
{/snippet}

{#if isEmpty}
	<!-- A typed empty state, not a blank card: "no results" is a real outcome
	     and should look deliberate. -->
	<div class="rows-empty">Nothing to show</div>
{:else}
	<div class="rows">
		<!-- Keyed by INDEX, not by title.
		     Titles are not unique and never were: `dnf search firefox` returns
		     three flatpak rows called "Firefox" (three remotes/versions), plus
		     three "Nvidia VAAPI driver" and two "Joplin". Svelte 5 treats a
		     duplicate key as fatal — `each_key_duplicate` threw during render,
		     so the panel painted nothing and the spinner ran forever while the
		     backend had already returned complete results in ~2.5s.
		     Index is the right key here: these lists are replaced wholesale on
		     each result, never reordered or incrementally patched, so there is
		     no identity to preserve across updates. -->
		{#each sections as section, si (si)}
			{#if section.title}
				<div class="section-title">{section.title}</div>
			{/if}
			{#each section.rows as row, ri (ri)}
				{@const primary = primaryAction(row)}
				<!-- An actionable row is a real <button>: keyboard activation, focus
				     and screen-reader semantics come for free instead of being
				     reimplemented with role + tabindex + a keydown handler. The two
				     cases are written out rather than switched with
				     `<svelte:element>` so the element type is statically known — a
				     dynamic tag defeats the a11y linter, which is the tool that
				     catches the next person getting this wrong. -->
				{#if primary}
					<button
						type="button"
						class="row actionable"
						onclick={() => onaction?.(section.handler, primary.id, primary.target)}
					>
						{@render rowBody(row)}
					</button>
				{:else}
					<div class="row">{@render rowBody(row)}</div>
				{/if}
			{/each}
		{/each}
	</div>
{/if}

<style>
	.rows {
		display: flex;
		flex-direction: column;
		/* No max-height or overflow here on purpose: the result panel is already
		   a scroll container (max-height 300px, overflow-y auto), and adding a
		   second one nests them — the user gets two scrollbars and has to scroll
		   the outer one to reach the end of the inner one. Scrolling is the
		   panel's job; this component just lays out rows. */
	}

	.rows-empty {
		padding: 16px 12px;
		font-size: 13px;
		color: var(--fg-muted);
		text-align: center;
	}

	.section-title {
		padding: 8px 12px 4px;
		font-size: 11px;
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: 0.04em;
		color: var(--fg-muted);
	}

	.row {
		/* Reset the UA button styling — these are rows that happen to be
		   buttons for accessibility, not buttons that look like rows. */
		appearance: none;
		background: none;
		border: none;
		font: inherit;
		text-align: left;
		width: 100%;
		display: flex;
		align-items: center;
		gap: 8px;
		padding: 7px 12px;
		border-radius: 6px;
	}

	.row.actionable:hover {
		background: var(--bg-hover);
		cursor: pointer;
	}

	.row-main {
		display: flex;
		align-items: baseline;
		gap: 8px;
		min-width: 0;
		flex: 1;
	}

	.row-title {
		font-size: 13px;
		color: var(--fg);
		white-space: nowrap;
	}

	.row-subtitle {
		font-size: 12px;
		color: var(--fg-muted);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	/* Tones are semantic and fixed, so a failed unit, a failed conversion and an
	   expired timer all read the same. */
	.badge {
		flex-shrink: 0;
		padding: 1px 7px;
		border-radius: 10px;
		font-size: 11px;
		font-weight: 500;
	}
	.badge-ok {
		background: color-mix(in srgb, var(--accent-ok, #3fb950) 18%, transparent);
		color: var(--accent-ok, #3fb950);
	}
	.badge-warn {
		background: color-mix(in srgb, var(--accent-warn, #d29922) 18%, transparent);
		color: var(--accent-warn, #d29922);
	}
	.badge-error {
		background: color-mix(in srgb, var(--accent-error, #f85149) 18%, transparent);
		color: var(--accent-error, #f85149);
	}
	.badge-muted {
		background: var(--bg-hover);
		color: var(--fg-muted);
	}

	.accessory {
		flex-shrink: 0;
		font-size: 11px;
		color: var(--fg-muted);
		font-variant-numeric: tabular-nums;
	}
</style>

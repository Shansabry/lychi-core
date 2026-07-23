<script lang="ts">
import { onMount } from "svelte";
import { type CommandInfo, getCommandCatalog, getTriggerCatalog } from "$lib/ipc";

let guideTab: "commands" | "triggers" = $state("commands");

// Both lists are generated from the LIVE action registry, so they never go
// stale — registering a handler makes it appear automatically, and its category
// comes from the handler's own `category()`. No hardcoded frontend mapping.
let commands: CommandInfo[] = $state([]);
let triggers: CommandInfo[] = $state([]);
let query = $state("");
let collapsed = $state<Set<string>>(new Set());
let searchEl: HTMLInputElement | undefined = $state();

onMount(async () => {
	try {
		[commands, triggers] = await Promise.all([getCommandCatalog(), getTriggerCatalog()]);
	} catch {
		commands = [];
		triggers = [];
	}
});

const active = $derived(guideTab === "commands" ? commands : triggers);

// A category key → its accent CSS variable. Falls back to General for anything
// the backend hasn't classified. Keyed on the enum string the DTO carries.
function catVar(cat: string): string {
	return `var(--cat-${cat})`;
}

// Filter by keyword or description (case-insensitive), then group by the
// backend-provided category (title + order travel with each row).
type Group = { key: string; title: string; order: number; color: string; rows: CommandInfo[] };

const groups = $derived.by<Group[]>(() => {
	const q = query.trim().toLowerCase();
	const filtered = q
		? active.filter(
				(c) => c.keyword.toLowerCase().includes(q) || c.description.toLowerCase().includes(q),
			)
		: active;
	const byKey = new Map<string, Group>();
	for (const row of filtered) {
		const key = row.category;
		let g = byKey.get(key);
		if (!g) {
			g = {
				key,
				title: row.category_title,
				order: row.category_order,
				color: catVar(row.category),
				rows: [],
			};
			byKey.set(key, g);
		}
		g.rows.push(row);
	}
	return [...byKey.values()].sort((a, b) => a.order - b.order || a.title.localeCompare(b.title));
});

const resultCount = $derived(groups.reduce((n, g) => n + g.rows.length, 0));

function toggleCollapse(key: string) {
	const next = new Set(collapsed);
	if (next.has(key)) next.delete(key);
	else next.add(key);
	collapsed = next;
}

// Highlight the matched substring in a piece of text (returns [before, hit, after]).
function splitMatch(text: string): [string, string, string] {
	const q = query.trim();
	if (!q) return [text, "", ""];
	const i = text.toLowerCase().indexOf(q.toLowerCase());
	if (i < 0) return [text, "", ""];
	return [text.slice(0, i), text.slice(i, i + q.length), text.slice(i + q.length)];
}

function handleKeydown(e: KeyboardEvent) {
	// `/` focuses search (unless already typing in it).
	if (e.key === "/" && e.target !== searchEl) {
		e.preventDefault();
		searchEl?.focus();
		return;
	}
	// Esc clears the search when it has content (and stays in the panel).
	if (e.key === "Escape" && query) {
		e.preventDefault();
		e.stopPropagation();
		query = "";
		return;
	}
	// Tab (no modifier) switches Commands ↔ Triggers when not in the search box.
	if (e.key === "Tab" && !e.shiftKey && e.target !== searchEl) {
		e.preventDefault();
		guideTab = guideTab === "commands" ? "triggers" : "commands";
	}
}
</script>

<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<div class="guide" role="region" aria-label="Guide" onkeydown={handleKeydown}>
	<div class="ghead">
		<div class="search">
			<span class="ico" aria-hidden="true">⌕</span>
			<input
				bind:this={searchEl}
				bind:value={query}
				type="text"
				placeholder="Search commands…"
				spellcheck="false"
				aria-label="Search commands"
			/>
			{#if query}
				<span class="rescount">{resultCount} result{resultCount === 1 ? "" : "s"}</span>
			{:else}
				<kbd class="slash">/</kbd>
			{/if}
		</div>
		<div class="seg-toggle" role="tablist">
			<button
				class:on={guideTab === "commands"}
				onmousedown={(e) => e.preventDefault()}
				onclick={() => (guideTab = "commands")}
				tabindex={-1}
				role="tab"
				aria-selected={guideTab === "commands"}>Commands</button
			>
			<button
				class:on={guideTab === "triggers"}
				onmousedown={(e) => e.preventDefault()}
				onclick={() => (guideTab = "triggers")}
				tabindex={-1}
				role="tab"
				aria-selected={guideTab === "triggers"}>Triggers</button
			>
		</div>
	</div>

	<div class="gbody">
		{#each groups as g (g.key)}
			{@const isCollapsed = collapsed.has(g.key)}
			<div class="cat" class:collapsed={isCollapsed} style="--kwc: {g.color}">
				<button
					class="cat-head"
					onmousedown={(e) => e.preventDefault()}
					onclick={() => toggleCollapse(g.key)}
					tabindex={-1}
				>
					<span class="cat-tri" aria-hidden="true">▾</span>
					<span class="cat-dot" aria-hidden="true"></span>
					<span class="cat-name">{g.title}</span>
					<span class="cat-count">{g.rows.length}</span>
				</button>
				{#if !isCollapsed}
					<div class="cat-rows">
						{#each g.rows as row (row.keyword + row.id)}
							{@const kw = splitMatch(row.keyword)}
							{@const ds = splitMatch(row.description)}
							<div class="row">
								<code>{kw[0]}{#if kw[1]}<mark>{kw[1]}</mark>{/if}{kw[2]}</code>
								<span class="desc">{ds[0]}{#if ds[1]}<mark>{ds[1]}</mark>{/if}{ds[2]}</span>
							</div>
						{/each}
					</div>
				{/if}
			</div>
		{:else}
			<div class="guide-empty">
				{#if query}
					No commands match “{query}”.
				{:else}
					Loading {guideTab}…
				{/if}
			</div>
		{/each}
	</div>

	<div class="keyhints">
		<span class="kh"><kbd>/</kbd> search</span>
		<span class="kh"><kbd>Tab</kbd> commands / triggers</span>
		{#if query}<span class="kh"><kbd>Esc</kbd> clear</span>{/if}
	</div>
</div>

<style>
	.guide {
		display: flex;
		flex-direction: column;
		padding: 0;
	}

	/* --- header: search + segmented toggle, sticky under the panel scroll --- */
	.ghead {
		display: flex;
		align-items: center;
		gap: 10px;
		padding: 2px 0 12px;
		position: sticky;
		top: 0;
		background: var(--bg-secondary);
		z-index: 2;
	}
	.search {
		flex: 1;
		display: flex;
		align-items: center;
		gap: 8px;
		background: var(--bg);
		border: 1px solid var(--border);
		border-radius: 8px;
		padding: 7px 11px;
		min-width: 0;
	}
	.search:focus-within {
		border-color: var(--fg-muted);
	}
	.search .ico {
		color: var(--fg-muted);
		font-size: 13px;
		flex-shrink: 0;
	}
	.search input {
		flex: 1;
		min-width: 0;
		background: none;
		border: none;
		outline: none;
		color: var(--fg);
		font-family: var(--font-mono);
		font-size: 12px;
	}
	.rescount {
		font-family: var(--font-mono);
		font-size: 10px;
		color: var(--fg-muted);
		flex-shrink: 0;
	}
	.slash {
		font-family: var(--font-mono);
		font-size: 10px;
		color: var(--fg-muted);
		background: var(--bg-secondary);
		border: 1px solid var(--border);
		border-radius: 4px;
		padding: 0 5px;
		flex-shrink: 0;
	}
	.seg-toggle {
		display: flex;
		background: var(--bg);
		border: 1px solid var(--border);
		border-radius: 8px;
		overflow: hidden;
		flex-shrink: 0;
	}
	.seg-toggle button {
		background: none;
		border: none;
		color: var(--fg-muted);
		font-family: var(--font-mono);
		font-size: 10.5px;
		text-transform: uppercase;
		letter-spacing: 0.4px;
		padding: 7px 11px;
		cursor: pointer;
		transition: color 100ms ease, background 100ms ease;
	}
	.seg-toggle button:hover {
		color: var(--fg);
	}
	.seg-toggle button.on {
		background: var(--bg-secondary);
		color: var(--fg);
	}

	.gbody {
		display: flex;
		flex-direction: column;
	}

	/* --- category section --- */
	.cat {
		margin-top: 14px;
	}
	.cat:first-child {
		margin-top: 6px;
	}
	.cat-head {
		display: flex;
		align-items: center;
		gap: 9px;
		width: 100%;
		background: none;
		border: none;
		padding: 6px 2px;
		cursor: pointer;
		color: var(--fg);
		text-align: left;
	}
	.cat-tri {
		color: var(--fg-muted);
		font-size: 9px;
		transition: transform 0.15s ease;
		flex-shrink: 0;
		width: 10px;
	}
	.cat.collapsed .cat-tri {
		transform: rotate(-90deg);
	}
	.cat-dot {
		width: 9px;
		height: 9px;
		border-radius: 3px;
		background: var(--kwc);
		flex-shrink: 0;
		box-shadow: 0 0 8px color-mix(in srgb, var(--kwc) 40%, transparent);
	}
	.cat-name {
		font-size: 12.5px;
		font-weight: 600;
		letter-spacing: 0.2px;
	}
	.cat-count {
		margin-left: auto;
		font-family: var(--font-mono);
		font-size: 10px;
		color: var(--fg-muted);
		background: var(--bg);
		border: 1px solid var(--border);
		border-radius: 10px;
		padding: 1px 7px;
	}

	.cat-rows {
		display: flex;
		flex-direction: column;
		gap: 1px;
		padding-left: 28px;
		margin-top: 2px;
	}
	.row {
		display: grid;
		grid-template-columns: 128px 1fr;
		align-items: baseline;
		gap: 14px;
		padding: 4px 8px;
		border-radius: 6px;
	}
	.row:hover {
		background: var(--bg);
	}
	.row code {
		font-family: var(--font-mono);
		font-size: 11.5px;
		color: var(--fg);
		background: var(--bg);
		border: 1px solid var(--border);
		border-left: 2px solid var(--kwc);
		border-radius: 4px;
		padding: 2px 7px;
		justify-self: start;
		white-space: nowrap;
		max-width: 100%;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.row .desc {
		font-size: 12px;
		color: var(--fg-muted);
		line-height: 1.45;
	}
	mark {
		background: color-mix(in srgb, var(--ai) 30%, transparent);
		color: var(--fg);
		border-radius: 2px;
		padding: 0 1px;
	}

	.guide-empty {
		font-size: 12px;
		color: var(--fg-muted);
		padding: 24px 4px;
		text-align: center;
	}

	/* --- keyhints footer --- */
	.keyhints {
		display: flex;
		gap: 16px;
		padding: 12px 2px 2px;
		margin-top: 10px;
		border-top: 1px solid var(--border);
		color: var(--fg-muted);
		font-size: 11px;
	}
	.kh {
		display: inline-flex;
		align-items: center;
		gap: 5px;
	}
	.kh kbd {
		font-family: var(--font-mono);
		font-size: 10px;
		background: var(--bg);
		border: 1px solid var(--border);
		border-radius: 4px;
		padding: 0 5px;
	}
</style>

<script lang="ts">
import { Blocks, Puzzle, Sparkles } from "lucide-svelte";

// A pure teaser: the plugin ecosystem ships after launch (see the root project
// plan). This panel sets the expectation and hints at the shape of it, without a
// call-to-action or any wiring — nothing here talks to the backend. Copy stays
// plain-language and benefit-first — no architecture or developer jargon.
const pillars = [
	{
		icon: Puzzle,
		title: "Add what you need",
		body: "Browse a library of ready-made plugins and add the ones you want with a single click. New shortcuts, searches, and actions — no setup.",
	},
	{
		icon: Sparkles,
		title: "Make Lychi yours",
		body: "Keep the launcher lean and turn on only what fits how you work. Everything else stays out of your way.",
	},
];
</script>

<div class="plugins">
	<div class="hero">
		<div class="hero-icon">
			<Blocks size={40} strokeWidth={1.25} />
		</div>
		<span class="badge">Under construction</span>
		<h2 class="hero-title">Plugins</h2>
		<p class="hero-sub">
			Lychi already does a lot on its own. Soon you'll be able to add more —
			handy plugins that give it new tricks, whenever you want them.
		</p>
	</div>

	<div class="pillars">
		{#each pillars as p (p.title)}
			<div class="pillar">
				<div class="pillar-icon">
					<p.icon size={16} strokeWidth={1.5} />
				</div>
				<div class="pillar-text">
					<span class="pillar-title">{p.title}</span>
					<span class="pillar-body">{p.body}</span>
				</div>
			</div>
		{/each}
	</div>

	<p class="foot">
		Local-first and privacy-preserving, like everything else in Lychi. We'll
		announce it at lychi.app when it's ready.
	</p>
</div>

<style>
	.plugins {
		display: flex;
		flex-direction: column;
		gap: 16px;
		padding: 4px 0;
	}

	.hero {
		display: flex;
		flex-direction: column;
		align-items: center;
		text-align: center;
		gap: 6px;
		padding: 8px 0 4px;
	}

	.hero-icon {
		color: var(--accent, var(--fg));
		opacity: 0.9;
		margin-bottom: 2px;
	}

	.badge {
		font-size: 10px;
		letter-spacing: 0.06em;
		text-transform: uppercase;
		color: var(--accent, var(--fg-muted));
		background: color-mix(in srgb, var(--accent, var(--fg)) 14%, transparent);
		padding: 2px 10px;
		border-radius: 10px;
	}

	.hero-title {
		font-size: 22px;
		font-weight: 300;
		letter-spacing: 0.06em;
		color: var(--fg);
		margin: 2px 0 0 0;
		font-family: var(--font-brand);
	}

	.hero-sub {
		font-size: 12px;
		color: var(--fg-muted);
		line-height: 1.55;
		margin: 0;
		max-width: 34ch;
	}

	.pillars {
		display: flex;
		flex-direction: column;
		gap: 8px;
	}

	.pillar {
		display: flex;
		align-items: flex-start;
		gap: 10px;
		padding: 10px 12px;
		background: var(--bg-secondary);
		border-radius: 6px;
		/* Subtly de-emphasized: this is a preview, not a live surface. */
		opacity: 0.92;
	}

	.pillar-icon {
		color: var(--fg-muted);
		flex-shrink: 0;
		margin-top: 1px;
	}

	.pillar-text {
		display: flex;
		flex-direction: column;
		gap: 3px;
	}

	.pillar-title {
		font-size: 12px;
		font-weight: 600;
		color: var(--fg);
	}

	.pillar-body {
		font-size: 11.5px;
		color: var(--fg-muted);
		line-height: 1.5;
	}

	.foot {
		font-size: 11px;
		color: var(--fg-muted);
		line-height: 1.5;
		margin: 0;
		text-align: center;
		opacity: 0.75;
	}
</style>

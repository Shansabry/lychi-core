<script lang="ts">
import { type Estimate, getModelPotential } from "$lib/ipc";
import ExperimentalNote from "./ExperimentalNote.svelte";

// `refreshKey` changes whenever the model/mode changes; the effect re-fetches.
// The backend computes the estimate on save and stores it, but also computes it
// live if none is stored — so this always resolves to something for a real model.
let { refreshKey = "" }: { refreshKey?: string } = $props();

let estimate = $state<Estimate | null>(null);

$effect(() => {
	// Read refreshKey so the effect re-runs when the model/mode changes.
	void refreshKey;
	let cancelled = false;
	getModelPotential()
		.then((e) => {
			if (!cancelled) estimate = e;
		})
		.catch(() => {
			if (!cancelled) estimate = null;
		});
	return () => {
		cancelled = true;
	};
});

// 1 / 2 / 3 segments filled.
const filled = $derived(
	estimate?.tier === "full" ? 3 : estimate?.tier === "capable" ? 2 : estimate ? 1 : 0,
);
const tierLabel = $derived(
	estimate?.tier === "full"
		? "Full"
		: estimate?.tier === "capable"
			? "Capable"
			: estimate
				? "Basic"
				: "",
);
// "3B · Q4" — only the parts that are known.
const signals = $derived(
	[estimate?.params_label, estimate?.quant_label].filter(Boolean).join(" · "),
);
const isBasic = $derived(estimate?.tier === "basic");
</script>

{#if estimate}
	<div class="meter" role="group" aria-label="Model capability estimate">
		<span class="label">Potential</span>
		<span class="bar" aria-hidden="true">
			{#each [0, 1, 2] as i (i)}
				<span
					class="seg"
					class:on={i < filled}
					class:basic={isBasic}
					class:full={estimate.tier === "full"}
				></span>
			{/each}
		</span>
		<span class="tier" class:basic={isBasic}>{tierLabel}</span>
		{#if signals}<span class="signals">{signals}</span>{/if}
		<span class="est">estimate</span>
	</div>

	{#if isBasic}
		<ExperimentalNote>
			This model is on the smaller side, so expect simpler reasoning and the
			occasional miss on complex commands. It's a rough estimate from the model's
			size and quantization — a larger model or a cloud provider gives the full
			Lychi experience.
		</ExperimentalNote>
	{/if}
{/if}

<style>
	.meter {
		display: flex;
		align-items: center;
		gap: 10px;
		padding: 10px 2px 2px;
		font-size: 12px;
		color: var(--fg-muted);
	}
	.label {
		font-weight: 600;
		color: var(--fg);
	}
	.bar {
		display: inline-flex;
		gap: 3px;
	}
	.seg {
		width: 22px;
		height: 6px;
		border-radius: 3px;
		background: color-mix(in srgb, var(--fg) 12%, var(--bg));
	}
	.seg.on {
		background: var(--accent);
	}
	.seg.on.basic {
		background: var(--warning);
	}
	.tier {
		font-weight: 600;
		color: var(--accent);
	}
	.tier.basic {
		color: var(--warning);
	}
	.signals {
		font-family: var(--font-mono);
		font-size: 11px;
	}
	.est {
		margin-left: auto;
		font-size: 10px;
		opacity: 0.6;
		text-transform: uppercase;
		letter-spacing: 0.05em;
	}
</style>

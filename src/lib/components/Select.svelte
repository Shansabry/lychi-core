<script lang="ts">
import { Coins } from "lucide-svelte";

type Option = { value: string; label: string; disabled?: boolean };

let {
	id = "",
	value = "",
	options = [] as Option[],
	onchange,
}: {
	id?: string;
	value: string;
	options: Option[];
	onchange: (value: string) => void;
} = $props();

let open = $state(false);
let openUp = $state(false);
let buttonEl: HTMLButtonElement | undefined = $state();
let listEl: HTMLDivElement | undefined = $state();
let dropdownStyle = $state("");

function parseTier(label: string): { tier: number; text: string } {
	const match = label.match(/^(\$+)\s+(.+)$/);
	if (!match) return { tier: 0, text: label };
	return { tier: match[1].length, text: match[2] };
}

let selectedLabel = $derived(options.find((o) => o.value === value)?.label ?? value);
let selectedParsed = $derived(parseTier(selectedLabel));

function positionDropdown() {
	if (!buttonEl) return;
	const rect = buttonEl.getBoundingClientRect();
	const spaceBelow = window.innerHeight - rect.bottom;
	openUp = spaceBelow < 220;

	if (openUp) {
		dropdownStyle = `position:fixed;left:${rect.left}px;bottom:${window.innerHeight - rect.top + 2}px;width:${rect.width}px;`;
	} else {
		dropdownStyle = `position:fixed;left:${rect.left}px;top:${rect.bottom + 2}px;width:${rect.width}px;`;
	}
}

function toggle() {
	if (!open) {
		positionDropdown();
	}
	open = !open;
}

function select(opt: Option) {
	if (opt.disabled) return; // e.g. "coming soon" — visible but not selectable
	open = false;
	if (opt.value !== value) {
		onchange(opt.value);
	}
}

function handleKeydown(e: KeyboardEvent) {
	if (!open) {
		if (e.key === "Enter" || e.key === " " || e.key === "ArrowDown") {
			e.preventDefault();
			positionDropdown();
			open = true;
		}
		return;
	}

	if (e.key === "Escape") {
		e.preventDefault();
		e.stopPropagation();
		open = false;
		buttonEl?.focus();
		return;
	}

	if (e.key === "ArrowDown" || e.key === "ArrowUp") {
		e.preventDefault();
		const idx = options.findIndex((o) => o.value === value);
		const step = e.key === "ArrowDown" ? 1 : -1;
		// Skip disabled options (e.g. "coming soon").
		let next = idx;
		for (let i = idx + step; i >= 0 && i < options.length; i += step) {
			if (!options[i].disabled) {
				next = i;
				break;
			}
		}
		if (next !== idx) {
			onchange(options[next].value);
		}
		return;
	}

	if (e.key === "Enter" || e.key === " ") {
		e.preventDefault();
		open = false;
		buttonEl?.focus();
	}
}

function handleBlur(e: FocusEvent) {
	const related = e.relatedTarget as Node | null;
	if (related && listEl?.contains(related)) return;
	open = false;
}
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="select-wrapper" onkeydown={handleKeydown}>
	<button
		{id}
		class="select-trigger"
		class:open
		type="button"
		bind:this={buttonEl}
		onclick={toggle}
		onblur={handleBlur}
		aria-haspopup="listbox"
		aria-expanded={open}
	>
		<span class="select-value">
			{selectedParsed.text}
			{#if selectedParsed.tier > 0}
				<span class="tier-icons">
					{#each Array(selectedParsed.tier) as _}
						<Coins size={11} />
					{/each}
				</span>
			{/if}
		</span>
		<svg class="select-chevron" width="10" height="6" viewBox="0 0 10 6" fill="none">
			<path d="M1 1L5 5L9 1" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"/>
		</svg>
	</button>

	{#if open}
		<div class="select-dropdown" style={dropdownStyle} bind:this={listEl} role="listbox">
			{#each options as opt}
				{@const parsed = parseTier(opt.label)}
				<button
					class="select-option"
					class:selected={opt.value === value}
					class:disabled={opt.disabled}
					type="button"
					role="option"
					aria-selected={opt.value === value}
					aria-disabled={opt.disabled}
					onclick={() => select(opt)}
					onblur={handleBlur}
				>
					<span class="option-text">{parsed.text}</span>
					{#if parsed.tier > 0}
						<span class="tier-icons">
							{#each Array(parsed.tier) as _}
								<Coins size={11} />
							{/each}
						</span>
					{/if}
				</button>
			{/each}
		</div>
	{/if}
</div>

<style>
	.select-wrapper {
		position: relative;
		flex: 1;
		min-width: 0;
	}

	.select-trigger {
		width: 100%;
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 8px;
		background: var(--bg-secondary);
		color: var(--fg);
		border: 1px solid var(--border);
		border-radius: 4px;
		padding: 5px 8px;
		font-family: var(--font-mono);
		font-size: 12px;
		cursor: pointer;
		outline: none;
		text-align: left;
	}

	.select-trigger:focus {
		border-color: var(--fg-muted);
	}

	.select-trigger.open {
		border-color: var(--fg-muted);
	}

	.select-value {
		flex: 1;
		min-width: 0;
		overflow: hidden;
		white-space: nowrap;
		display: flex;
		align-items: center;
		gap: 4px;
	}

	.option-text {
		flex: 1;
	}

	.select-chevron {
		flex-shrink: 0;
		color: var(--fg-muted);
		transition: transform 100ms ease;
	}

	.select-trigger.open .select-chevron {
		transform: rotate(180deg);
	}

	.select-dropdown {
		background: var(--bg-secondary);
		border: 1px solid var(--border);
		border-radius: 4px;
		padding: 2px;
		z-index: 9999;
		max-height: 200px;
		overflow-y: auto;
	}

	.select-option {
		width: 100%;
		display: flex;
		align-items: center;
		gap: 4px;
		background: none;
		color: var(--fg);
		border: none;
		border-radius: 3px;
		padding: 5px 8px;
		font-family: var(--font-mono);
		font-size: 12px;
		cursor: pointer;
		text-align: left;
		outline: none;
	}

	.select-option:hover,
	.select-option:focus {
		background: var(--border);
	}

	.select-option.selected {
		color: var(--accent);
	}

	.select-option.disabled {
		color: var(--fg-muted);
		opacity: 0.55;
		cursor: default;
	}
	.select-option.disabled:hover,
	.select-option.disabled:focus {
		background: none;
	}

	.tier-icons {
		display: inline-flex;
		align-items: center;
		gap: 1px;
		color: var(--fg-muted);
		flex-shrink: 0;
		margin-left: auto;
	}
</style>

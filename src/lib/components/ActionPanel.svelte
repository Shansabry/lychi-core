<script lang="ts">
/**
 * ⌘K secondary-actions panel — a small floating menu of actions for the
 * currently-selected result. Modeled on Select.svelte (dropdown nav) + the
 * confirm-panel modal pattern (autofocus + stopPropagation so it intercepts
 * keys before the completion list). The action IMPLEMENTATIONS live in
 * +page.svelte; this panel only lists them and reports the chosen one.
 */
import { matchesAction } from "$lib/keybindings";

export type ActionItem = {
	/** Stable id the parent switches on to run the action. */
	id: string;
	/** Menu label. */
	label: string;
	/** Optional right-aligned shortcut hint (e.g. "Ctrl+Enter"). */
	hint?: string;
};

let {
	actions = [] as ActionItem[],
	onrun,
	ondismiss,
}: {
	actions: ActionItem[];
	onrun: (id: string) => void;
	ondismiss: () => void;
} = $props();

let selected = $state(0);
let panelEl: HTMLDivElement | undefined = $state();

// Focus the panel on mount so it captures keys before the input/completion list.
function autofocus(node: HTMLElement) {
	requestAnimationFrame(() => node.focus());
}

function handleKeydown(e: KeyboardEvent) {
	// Ctrl+K again toggles the panel closed (matches the open shortcut).
	if (matchesAction(e, "action_panel")) {
		e.preventDefault();
		e.stopPropagation();
		ondismiss();
		return;
	}
	if (e.key === "Escape") {
		e.preventDefault();
		e.stopPropagation();
		ondismiss();
		return;
	}
	if (e.key === "ArrowDown") {
		e.preventDefault();
		e.stopPropagation();
		selected = Math.min(selected + 1, actions.length - 1);
		return;
	}
	if (e.key === "ArrowUp") {
		e.preventDefault();
		e.stopPropagation();
		selected = Math.max(selected - 1, 0);
		return;
	}
	if (e.key === "Enter" || e.key === " ") {
		e.preventDefault();
		e.stopPropagation();
		const a = actions[selected];
		if (a) onrun(a.id);
		return;
	}
	// Any other key (typing) closes the panel and returns to the input.
	if (e.key.length === 1 && !e.ctrlKey && !e.metaKey && !e.altKey) {
		ondismiss();
	}
}

// Close if focus leaves the panel (e.g. click elsewhere).
function handleBlur(e: FocusEvent) {
	const related = e.relatedTarget as Node | null;
	if (related && panelEl?.contains(related)) return;
	ondismiss();
}
</script>

<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<div
	class="action-panel"
	bind:this={panelEl}
	use:autofocus
	tabindex="-1"
	role="menu"
	aria-label="Result actions"
	onkeydown={handleKeydown}
	onblur={handleBlur}
>
	<div class="action-header">Actions</div>
	{#each actions as a, i (a.id)}
		<button
			class="action-option"
			class:selected={i === selected}
			type="button"
			role="menuitem"
			tabindex="-1"
			onmouseenter={() => (selected = i)}
			onclick={() => onrun(a.id)}
		>
			<span class="action-label">{a.label}</span>
			{#if a.hint}
				<span class="action-hint">{a.hint}</span>
			{/if}
		</button>
	{/each}
</div>

<style>
	.action-panel {
		/* Anchored to the launcher (main is position:relative), NOT the viewport —
		   sits in the bottom-right of the window just above the status bar and
		   grows upward, like Raycast's action menu. */
		position: absolute;
		right: 12px;
		bottom: 44px;
		min-width: 240px;
		max-width: 340px;
		max-height: 320px;
		overflow-y: auto;
		background: var(--bg-secondary);
		border: 1px solid var(--border);
		border-radius: 8px;
		padding: 4px;
		z-index: 100;
		box-shadow: 0 -6px 24px rgba(0, 0, 0, 0.45);
		outline: none;
	}

	.action-header {
		font-family: var(--font-mono);
		font-size: 10px;
		text-transform: uppercase;
		letter-spacing: 0.5px;
		color: var(--fg-muted);
		padding: 4px 8px 6px;
	}

	.action-option {
		width: 100%;
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 12px;
		background: none;
		color: var(--fg);
		border: none;
		border-radius: 4px;
		padding: 6px 8px;
		font-family: var(--font-mono);
		font-size: 12px;
		cursor: pointer;
		text-align: left;
		outline: none;
	}

	.action-option.selected {
		background: var(--border);
	}

	.action-label {
		flex: 1;
		min-width: 0;
		overflow: hidden;
		white-space: nowrap;
		text-overflow: ellipsis;
	}

	.action-hint {
		flex-shrink: 0;
		color: var(--fg-muted);
		font-size: 11px;
	}
</style>

<script lang="ts">
import type { BannerMode } from "$lib/onboarding";

/**
 * The first-run banner.
 *
 * Extracted from `+page.svelte` so the three modes can be rendered in a test —
 * the previous inline version could only be checked by reading it, and the one
 * thing it must never do again is show nothing.
 *
 * It renders no state of its own and decides nothing: `mode` comes from
 * `onboarding.bannerMode`, and `text` is the backend's own explanation of the
 * hotkey verdict. Re-deriving either here is what let the X11 case go
 * unmentioned in the first place.
 */
let {
	mode,
	text = "",
	copied = false,
	oncopy,
	onopensettings,
	ondismiss,
}: {
	mode: BannerMode;
	text?: string;
	copied?: boolean;
	oncopy: () => void;
	onopensettings: (tab: "general" | "guide") => void;
	ondismiss: () => void;
} = $props();
</script>

<div class="hotkey-banner">
	{#if mode === "welcome"}
		<span class="hotkey-banner-text">
			Welcome to Lychi. Type to search apps and files, or press
			<kbd>Tab</kbd> to complete — the Guide lists every command.
		</span>
		<button class="hotkey-banner-btn" onclick={() => onopensettings("guide")}>
			Open Guide
		</button>
	{:else if mode === "confirm"}
		<span class="hotkey-banner-text">
			Close this and press your hotkey to check it opens Lychi. If nothing happens,
			another app has claimed the key.
		</span>
		<button class="hotkey-banner-btn" onclick={() => onopensettings("general")}>
			Change hotkey
		</button>
		<button class="hotkey-banner-btn" onclick={oncopy}>
			{copied ? "Copied" : "Copy command"}
		</button>
	{:else}
		<span class="hotkey-banner-text">
			{text}. Pick another combination, or bind
			<code>lychi --toggle</code> in your desktop's keyboard settings.
		</span>
		<button class="hotkey-banner-btn" onclick={() => onopensettings("general")}>
			Change hotkey
		</button>
		<button class="hotkey-banner-btn" onclick={oncopy}>
			{copied ? "Copied" : "Copy command"}
		</button>
	{/if}
	<button class="hotkey-banner-btn dismiss" onclick={ondismiss}>Got it</button>
</div>

<style>
	.hotkey-banner {
		display: flex;
		align-items: center;
		gap: 8px;
		padding: 8px 12px;
		font-size: 11px;
		color: var(--fg-muted);
		background: var(--bg-secondary);
		border-bottom: 1px solid var(--border);
	}

	.hotkey-banner-text {
		flex: 1;
		line-height: 1.4;
	}

	.hotkey-banner-text code {
		font-family: var(--font-mono);
		background: var(--bg);
		padding: 1px 4px;
		border-radius: 3px;
		user-select: all;
	}

	/* A key to press, not a command to copy — so no `user-select: all`, which
	   would make double-clicking "Tab" select it as if it were pasteable. */
	.hotkey-banner-text kbd {
		font-family: var(--font-mono);
		background: var(--bg);
		border: 1px solid var(--border);
		padding: 0 4px;
		border-radius: 3px;
		font-size: 10px;
	}

	.hotkey-banner-btn {
		background: transparent;
		color: var(--accent);
		border: 1px solid var(--border);
		border-radius: 4px;
		padding: 3px 8px;
		font-size: 11px;
		cursor: pointer;
		flex-shrink: 0;
		transition: background 100ms ease;
	}

	.hotkey-banner-btn:hover {
		background: var(--border);
	}

	.hotkey-banner-btn.dismiss {
		color: var(--fg-muted);
	}
</style>

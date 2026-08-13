<script lang="ts">
import "../app.css";
import { onMount } from "svelte";
import { getGeneralConfig } from "$lib/ipc";
import { applyTheme, type Theme, type ThemeMode } from "$lib/theme";

let { children } = $props();

// The last-applied theme, so the OS-scheme listener can re-apply it (re-resolving
// the per-theme accent) when "system" is active and the desktop flips light/dark.
let current: Theme | null = null;

// Re-apply the whole theme when any theming setting changes. The event carries
// the full Theme so the engine is the single applier (no scattered dataset code).
function handleThemeChange(e: Event) {
	current = (e as CustomEvent<Theme>).detail;
	applyTheme(current);
}

onMount(() => {
	getGeneralConfig().then((config) => {
		current = {
			mode: (config.theme as ThemeMode) ?? "system",
			accent: config.accent ?? "",
			fontFamily: config.font_family ?? "",
			opacity: config.card_opacity ?? 1,
			cornerRadius: config.corner_radius ?? 12,
			blur: config.card_blur ?? false,
		};
		applyTheme(current);
	});

	window.addEventListener("lychi-theme-change", handleThemeChange);

	// When the mode is "system", re-apply on OS light/dark flips so the accent's
	// per-theme value tracks it (the CSS variables switch automatically via media
	// queries; only the JS-set --accent needs re-resolving).
	const mql = window.matchMedia?.("(prefers-color-scheme: light)");
	const onScheme = () => {
		if (current && current.mode === "system") applyTheme(current);
	};
	mql?.addEventListener?.("change", onScheme);

	return () => {
		window.removeEventListener("lychi-theme-change", handleThemeChange);
		mql?.removeEventListener?.("change", onScheme);
	};
});
</script>

{@render children()}

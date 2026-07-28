<script lang="ts">
import "../app.css";
import { onMount } from "svelte";
import { getGeneralConfig } from "$lib/ipc";
import { applyTheme, type Theme, type ThemeMode } from "$lib/theme";

let { children } = $props();

// Re-apply the whole theme when any theming setting changes. The event carries
// the full Theme so the engine is the single applier (no scattered dataset code).
function handleThemeChange(e: Event) {
	applyTheme((e as CustomEvent<Theme>).detail);
}

onMount(() => {
	getGeneralConfig().then((config) => {
		applyTheme({
			mode: (config.theme as ThemeMode) ?? "dark",
			accent: config.accent ?? "",
			fontFamily: config.font_family ?? "",
		});
	});

	window.addEventListener("lychi-theme-change", handleThemeChange);
	return () => window.removeEventListener("lychi-theme-change", handleThemeChange);
});
</script>

{@render children()}

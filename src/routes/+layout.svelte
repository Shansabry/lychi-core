<script lang="ts">
import "../app.css";
import { onMount } from "svelte";
import { getGeneralConfig } from "$lib/ipc";

let { children } = $props();

function applyTheme(theme: string) {
	document.documentElement.dataset.theme = theme;
}

function handleThemeChange(e: Event) {
	applyTheme((e as CustomEvent<string>).detail);
}

onMount(() => {
	getGeneralConfig().then((config) => applyTheme(config.theme));

	window.addEventListener("lychi-theme-change", handleThemeChange);
	return () => window.removeEventListener("lychi-theme-change", handleThemeChange);
});
</script>

{@render children()}

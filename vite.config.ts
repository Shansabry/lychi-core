import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig, type Plugin } from 'vite';

// SvelteKit still passes optimizeDeps.esbuildOptions, which Vite 8 (Rolldown)
// deprecates with a startup warning at config-merge time. The only thing
// SvelteKit puts there is a prebundle plugin for its experimental
// remoteFunctions feature, which this project doesn't enable — the option is
// a no-op here, so strip it from the hook's result before Vite merges it.
// Remove once @sveltejs/kit passes optimizeDeps.rolldownOptions natively.
async function sveltekitWithoutEsbuildOptions(): Promise<Plugin[]> {
	const plugins = await sveltekit();
	return plugins.map((plugin) => {
		if (plugin.name !== 'vite-plugin-sveltekit-setup' || typeof plugin.config !== 'function') {
			return plugin;
		}
		const originalConfig = plugin.config;
		return {
			...plugin,
			config(config, env) {
				const result = originalConfig.call(this, config, env);
				return Promise.resolve(result).then((resolved) => {
					if (resolved?.optimizeDeps && 'esbuildOptions' in resolved.optimizeDeps) {
						delete resolved.optimizeDeps.esbuildOptions;
					}
					return resolved;
				});
			},
		};
	});
}

export default defineConfig({
	plugins: [sveltekitWithoutEsbuildOptions()],
	clearScreen: false,
	server: {
		port: 42352,
		strictPort: true,
		warmup: {
			clientFiles: [
				'src/routes/+page.svelte',
				'src/lib/components/CommandInput.svelte',
				'src/lib/components/CompletionsList.svelte',
				'src/lib/components/ResultPanel.svelte',
				'src/lib/components/MediaPanel.svelte',
			]
		}
	},
	optimizeDeps: {
		// Pre-bundle deps at startup so first render doesn't trigger lazy compilation
		include: [
			'@tauri-apps/api/core',
			'@tauri-apps/api/event',
			'lucide-svelte',
			'marked',
		]
	}
});

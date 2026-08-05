import { sveltekit } from '@sveltejs/kit/vite';
import { svelteTesting } from '@testing-library/svelte/vite';
import type { Plugin } from 'vite';
// Vitest's `defineConfig`, not Vite's — the latter has no `test` key in its
// types, so the config would type-error despite being valid at runtime.
import { defineConfig } from 'vitest/config';

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
	// `svelteTesting` no-ops outside VITEST, so it costs the dev server nothing.
	// It handles three things a hand-written `resolve.conditions` does not:
	// inserting `browser` BEFORE `node` (order matters — otherwise `mount()`
	// still resolves to Svelte's server stub), registering auto-cleanup, and
	// adding the library to `ssr.noExternal` so the Svelte plugin compiles it.
	plugins: [sveltekitWithoutEsbuildOptions(), svelteTesting()],
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
	},
	test: {
		// A DOM only where a DOM is needed. `environmentMatchGlobs` is gone in
		// Vitest 4, so the per-file `@vitest-environment jsdom` docblock is the
		// supported way to opt in: the ~117 pure-function tests keep running in
		// node (fast, no jsdom construction) and only `*.svelte.test.ts` files
		// pay for a document.
		//
		// Why a real DOM at all: `svelte/server` renders components without one,
		// but it renders to a STRING and never runs the client keyed-each
		// reconciler — the thing that throws `each_key_duplicate`. Verified by
		// restoring the original buggy `(row.title)` keying, under which every
		// SSR test still passed. A harness that cannot fail on the bug it exists
		// to catch is worse than none.
		environment: 'node',
		setupFiles: ['./src/lib/test-setup.ts'],
	}
});

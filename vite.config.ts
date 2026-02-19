import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vite';

export default defineConfig({
	plugins: [sveltekit()],
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

/**
 * Attachments — the files staged for the next AI turn, rendered as chips beside
 * the launcher input.
 *
 * This store holds ONLY UI state. Every decision about a file (what it is,
 * whether it's usable, which backend pipe it takes) comes from the backend's
 * `classify_files`, so the classification rules live in one place — the Rust
 * `files::attachment` brick — and can't drift from the extractors that consume
 * them. The store never inspects a path itself.
 *
 * Two pipes, chosen by `route`:
 *   • `vision` → the path goes in `agentChatStart`'s `images` argument, where the
 *     backend base64-encodes it into an image content block.
 *   • `text`   → the path is appended to the prompt as an `@`-reference, which the
 *     backend expands into inlined extracted text before building the session.
 * `unsupported` chips are shown (so the user can see why and remove them) but
 * are never sent.
 *
 * Replaced-wholesale array → `$state.raw`, matching the WebKit paint sensitivity
 * documented in the chat store.
 */

import {
	attachFromClipboard,
	classifyFiles,
	type FileAttachment,
	getModelVision,
	type ModelVision,
} from "$lib/ipc";

class Attachments {
	/** Staged files, in the order the user attached them. */
	items = $state.raw<FileAttachment[]>([]);
	/** True while a `classify_files` round-trip is in flight (thumbnail decode). */
	loading = $state(false);

	/**
	 * What we know about the selected model's image support. Refreshed whenever
	 * an image is staged. `"unknown"` is NOT a warning — it means try it.
	 */
	modelVision = $state<ModelVision>("unknown");

	/** Any chips staged at all (drives whether the tray renders). */
	get any(): boolean {
		return this.items.length > 0;
	}

	/** An image is staged for a model already known to reject images. */
	get visionMismatch(): boolean {
		return this.modelVision === "unsupported" && this.items.some((a) => a.route === "vision");
	}

	/** Chips that will actually be sent (i.e. not `unsupported`). */
	get usable(): FileAttachment[] {
		return this.items.filter((a) => a.route !== "unsupported");
	}

	/**
	 * Stage file paths. Classification and de-duplication are the backend's job;
	 * we merge its verdicts into whatever is already staged, dropping repeats so
	 * attaching the same file twice yields one chip.
	 */
	add = async (paths: string[]): Promise<void> => {
		if (paths.length === 0) return;
		this.loading = true;
		try {
			const classified = await classifyFiles(paths);
			const known = new Set(this.items.map((a) => a.path));
			const fresh = classified.filter((a) => !known.has(a.path));
			if (fresh.length > 0) this.items = [...this.items, ...fresh];
			this.#refreshVision();
		} catch {
			// A failed classify leaves the tray untouched — better than a chip that
			// lies about what it is. The user can retry the attach gesture.
		} finally {
			this.loading = false;
		}
	};

	/**
	 * Stage whatever the clipboard holds (the paste gesture). Returns true when
	 * something was staged, so the caller knows whether to let the normal text
	 * paste proceed — a copied FILE becomes a chip, copied TEXT stays a text paste.
	 */
	addFromClipboard = async (): Promise<boolean> => {
		this.loading = true;
		try {
			const classified = await attachFromClipboard();
			if (classified.length === 0) return false;
			const known = new Set(this.items.map((a) => a.path));
			const fresh = classified.filter((a) => !known.has(a.path));
			if (fresh.length > 0) this.items = [...this.items, ...fresh];
			this.#refreshVision();
			// Even an all-duplicate paste counts as handled — the user's files ARE
			// staged, so we shouldn't also dump their paths into the input box.
			return true;
		} catch {
			return false;
		} finally {
			this.loading = false;
		}
	};

	/**
	 * Re-check the selected model's image support. Called after staging, and only
	 * when an image is actually present — a doc-only tray needs no vision.
	 * Fire-and-forget: a failed lookup leaves the last verdict rather than
	 * inventing a warning.
	 */
	#refreshVision(): void {
		if (!this.items.some((a) => a.route === "vision")) return;
		getModelVision()
			.then((v) => {
				this.modelVision = v;
			})
			.catch(() => {});
	}

	/** Remove one chip by path (the × button, or Backspace on an empty input). */
	remove = (path: string): void => {
		this.items = this.items.filter((a) => a.path !== path);
	};

	/** Drop the most recently attached chip (Backspace on an empty input). */
	removeLast = (): void => {
		if (this.items.length > 0) this.items = this.items.slice(0, -1);
	};

	/** Clear the tray (after a run starts, or on dismiss/reset). */
	clear = (): void => {
		this.items = [];
	};

	/**
	 * The image paths for `agentChatStart`'s `images` argument.
	 */
	imagePaths = (): string[] => this.items.filter((a) => a.route === "vision").map((a) => a.path);

	/**
	 * Fold the text-route attachments into the prompt as `@`-references, which the
	 * backend expands into inlined extracted text. Returns `prompt` unchanged when
	 * nothing is staged, so non-attachment turns are byte-identical to before.
	 */
	applyToPrompt = (prompt: string): string => {
		const refs = this.items.filter((a) => a.route === "text").map((a) => `@${a.path}`);
		if (refs.length === 0) return prompt;
		// Refs lead so the model sees the material before the instruction — and so a
		// bare "summarize this" with a doc attached still reads as a complete ask.
		return `${refs.join(" ")} ${prompt}`.trim();
	};
}

/** The single app-wide attachment tray. */
export const attachments = new Attachments();

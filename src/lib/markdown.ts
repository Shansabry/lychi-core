// Markdown rendering for the AI chat — `marked` + highlight.js (common subset)
// for code blocks, then DOMPurify sanitization. Kept in one place so the chat
// (and any future markdown surface) renders identically.
//
// highlight.js `common` is ~35 popular languages (js/ts/rust/python/go/json/
// bash/…) — enough for a launcher's code answers without the full 190-language
// bundle. Auto-detection handles the model's frequently-unlabeled code fences.

import hljs from "highlight.js/lib/common";
import { marked } from "marked";
import { sanitizeMarkdown } from "$lib/sanitize";

// Override the code-block renderer to run highlight.js. `lang` is the fence
// label (```rust); when absent or unknown we auto-detect. Output carries
// `hljs`/`hljs-*` classes (styled in app.css), which the sanitizer keeps.
const renderer = new marked.Renderer();
renderer.code = ({ text, lang }: { text: string; lang?: string }): string => {
	const language = lang && hljs.getLanguage(lang) ? lang : undefined;
	let highlighted: string;
	let detected = language ?? "";
	try {
		if (language) {
			highlighted = hljs.highlight(text, { language }).value;
		} else {
			const auto = hljs.highlightAuto(text);
			highlighted = auto.value;
			detected = auto.language ?? "";
		}
	} catch {
		// Fall back to escaped plain text on any highlighter error.
		highlighted = text.replace(
			/[&<>]/g,
			(c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;" })[c] ?? c,
		);
	}
	const label = detected ? ` data-lang="${detected}"` : "";
	return `<pre${label}><code class="hljs language-${detected}">${highlighted}</code></pre>`;
};

marked.use({ renderer });

/** Render markdown to sanitized, syntax-highlighted HTML. */
export function renderMarkdown(src: string): string {
	if (!src) return "";
	try {
		return sanitizeMarkdown(marked.parse(src) as string);
	} catch {
		return src.replace(/[&<>]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;" })[c] ?? c);
	}
}

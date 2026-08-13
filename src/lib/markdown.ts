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

/**
 * Temporarily close markdown constructs left dangling by a MID-STREAM cut, so a
 * half-arrived answer parses to sane HTML instead of a broken layout.
 *
 * A streamed answer is parsed on every frame over the accumulated prefix, which
 * routinely ends inside a construct: an unclosed ```` ``` ```` fence makes marked
 * swallow the REST of the answer into one code block (and a long unwrapped line
 * blows the panel width out), and an open `[text](url` mid-link emits a
 * malformed `<a>`. The fix (the streamdown/remend approach): repair a COPY for
 * the parser only — the stored text is untouched, so when the real closer
 * arrives the output just updates. Deliberately minimal: only the constructs
 * that actually corrupt LAYOUT are closed; a stray `**` marked renders as
 * literal text, which is harmless and not worth the false-positive risk.
 */
export function repairStreamingMarkdown(src: string): string {
	let out = src;

	// 1. Unclosed fenced code block — the big one. An odd number of ```` ``` ````
	//    fence lines means we're inside a block; append a closer so it ends here
	//    instead of eating everything after it.
	const fences = (out.match(/^```/gm) ?? []).length;
	if (fences % 2 === 1) {
		out += "\n```";
	}

	// 2. Unterminated link `[text](url` on the LAST line — an open `(` after a
	//    `]` with no closing `)`. Marked otherwise emits a broken `<a>` with a
	//    partial href. Drop the trailing `](…` back to plain text until it closes.
	const lastNl = out.lastIndexOf("\n");
	const lastLine = out.slice(lastNl + 1);
	const openLink = lastLine.lastIndexOf("](");
	if (openLink !== -1 && !lastLine.slice(openLink).includes(")")) {
		// Re-render the dangling `](partial` as literal text: strip it for now.
		out = out.slice(0, lastNl + 1) + lastLine.slice(0, openLink);
	}

	return out;
}

/** Render markdown to sanitized, syntax-highlighted HTML. */
export function renderMarkdown(src: string): string {
	if (!src) return "";
	try {
		return sanitizeMarkdown(marked.parse(src) as string);
	} catch {
		return src.replace(/[&<>]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;" })[c] ?? c);
	}
}

/**
 * Render markdown that is still STREAMING — repairs dangling constructs first so
 * a partial answer doesn't flash a broken layout. Use `renderMarkdown` for
 * settled text (a prior turn, or once streaming ends).
 */
export function renderStreamingMarkdown(src: string): string {
	if (!src) return "";
	return renderMarkdown(repairStreamingMarkdown(src));
}

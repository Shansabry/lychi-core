import DOMPurify, { type Config } from "dompurify";

// Markdown rendering — structural HTML + inline formatting.
// Blocks all event handlers, scripts, forms, iframes.
const MARKDOWN_CONFIG: Config = {
	ALLOWED_TAGS: [
		"h1",
		"h2",
		"h3",
		"h4",
		"h5",
		"h6",
		"p",
		"br",
		"hr",
		"ul",
		"ol",
		"li",
		"blockquote",
		"pre",
		"code",
		"a",
		"strong",
		"em",
		"del",
		"s",
		"sub",
		"sup",
		"table",
		"thead",
		"tbody",
		"tr",
		"th",
		"td",
		"img",
		"details",
		"summary",
		"div",
		"span",
	],
	ALLOWED_ATTR: ["href", "src", "alt", "title", "class", "id", "align", "width", "height"],
	ALLOW_DATA_ATTR: false,
};

// Terminal output — only spans with style/class (ANSI colors + clickable files).
const TERMINAL_CONFIG: Config = {
	ALLOWED_TAGS: ["span"],
	ALLOWED_ATTR: ["style", "class", "data-filepath"],
	ALLOW_DATA_ATTR: false,
};

export function sanitizeMarkdown(html: string): string {
	return DOMPurify.sanitize(html, MARKDOWN_CONFIG);
}

export function sanitizeTerminal(html: string): string {
	return DOMPurify.sanitize(html, TERMINAL_CONFIG);
}

// Inline SVG (e.g. a generated QR code). Enables the SVG profile so vector
// shapes survive, while DOMPurify still strips scripts and event handlers.
const SVG_CONFIG: Config = {
	USE_PROFILES: { svg: true, svgFilters: false },
	ADD_ATTR: ["viewBox", "preserveAspectRatio"],
};

export function sanitizeSvg(svg: string): string {
	return DOMPurify.sanitize(svg, SVG_CONFIG);
}

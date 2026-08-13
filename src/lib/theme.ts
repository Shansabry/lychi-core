/**
 * Theming engine — the single place that turns theme settings into applied CSS.
 *
 * Lychi's look is CSS-variable-driven (see app.css). This module is the small,
 * extensible engine that maps a `Theme` onto `document.documentElement`:
 * `data-theme` for the light/dark base, plus per-variable overrides (currently
 * just `--accent`). Adding a future knob (a second accent shade, density, etc.)
 * is one entry here + one line in `applyTheme` — no refactor.
 *
 * Everything already reads `var(--accent)`, so overriding that one variable
 * recolors selection highlights, the prompt, and active states app-wide.
 */

export type ThemeMode = "dark" | "light" | "system";
/** A concrete rendered mode — "system" resolved to what the OS is showing.
 *  Accent/contrast math only ever operates on one of these two. */
export type ResolvedMode = "dark" | "light";

export interface Theme {
	mode: ThemeMode;
	/**
	 * The stored accent: either a swatch `id` (e.g. "blue" — resolved to a
	 * per-theme value), a raw hex (custom, WCAG-adjusted per theme), or "" for
	 * the theme's default monochrome accent.
	 */
	accent: string;
	/**
	 * The app's font family, or "" for the built-in stacks.
	 *
	 * ONE choice drives the whole interface — it retargets both `--font-sans`
	 * and `--font-mono`, so labels and the command input agree. Command
	 * *output* is deliberately excluded (it reads `--font-output`) because
	 * `git status` and `ls -la` lose their columns in a proportional face.
	 *
	 * Prepended to each stack rather than replacing it, so an uninstalled font
	 * falls back to the normal stack instead of to nothing.
	 */
	fontFamily?: string;
	/** Launcher card background opacity, 0.30–1.00. Drives `--card-opacity`. */
	opacity?: number;
	/** Launcher card corner radius in px, 0–24. Drives `--card-radius`. */
	cornerRadius?: number;
	/** Frosted glass on/off. Real blur is requested from the compositor in the
	 *  backend; this drives the CSS `--card-frost` tint fallback. */
	blur?: boolean;
}

/** Opacity is clamped to a readable floor — below this the text over the desktop
 *  is unusable — and to 1.0 (fully opaque). Shared by the UI and applyTheme. */
export const MIN_CARD_OPACITY = 0.3;
export const MAX_CORNER_RADIUS = 24;

/** A named accent swatch for the settings picker. */
export interface AccentSwatch {
	id: string;
	label: string;
	/** Applied `--accent` value on the DARK theme. "" = theme default (mono). */
	dark: string;
	/** Applied `--accent` value on the LIGHT theme. "" = theme default (mono). */
	light: string;
}

/**
 * Curated accent palette. Each swatch carries a PER-THEME value rather than one
 * color adjusted algorithmically — the pro approach (Radix, IBM Carbon): a color
 * that reads well on a dark surface is muddy/low-contrast on a light one, so they
 * need independently-tuned values. Light values are Radix Colors "step 11" (the
 * low-contrast *text* step, designed exactly for accent-as-text and WCAG-AA
 * against a light surface); dark values are brighter tints that pass on #0a0a0a.
 * The first entry is the default (no override → theme's monochrome accent).
 */
export const ACCENTS: AccentSwatch[] = [
	{ id: "default", label: "Default", dark: "", light: "" },
	// dark: bright tint (passes on #0a0a0a) · light: Radix step-11 (passes on #f5f5f5)
	{ id: "blue", label: "Blue", dark: "#7c8cff", light: "#0d74ce" },
	{ id: "purple", label: "Purple", dark: "#a78bfa", light: "#6550b9" },
	{ id: "green", label: "Green", dark: "#4ade80", light: "#218358" },
	{ id: "amber", label: "Amber", dark: "#fbbf24", light: "#ab6400" },
	{ id: "orange", label: "Orange", dark: "#fb923c", light: "#cc4e00" },
	{ id: "pink", label: "Pink", dark: "#f472b6", light: "#c2298a" },
	{ id: "teal", label: "Teal", dark: "#2dd4bf", light: "#008573" },
	{ id: "red", label: "Red", dark: "#f87171", light: "#ce2c31" },
];

/** Whether a string is a plausible hex color (#rgb or #rrggbb). */
export function isHexColor(s: string): boolean {
	return /^#([0-9a-fA-F]{3}|[0-9a-fA-F]{6})$/.test(s.trim());
}

// --- WCAG contrast: keep the accent legible in BOTH themes ---
//
// `--accent` is used mostly as TEXT color on the theme background, so a
// dark-tuned accent (e.g. #7c8cff) is unreadable / invisible on the light
// theme's near-white background. We enforce a WCAG contrast floor against the
// theme background by lightening (dark theme) or darkening (light theme) the
// accent until it passes. This adapts the SAME accent per theme rather than
// maintaining a second hardcoded palette.

/** The theme background `--accent` text is shown against, per mode. */
const THEME_BG: Record<ResolvedMode, [number, number, number]> = {
	dark: [0x0a, 0x0a, 0x0a],
	light: [0xf5, 0xf5, 0xf5],
};

/** WCAG AA for normal text. We target this for the accent-as-text usage. */
const MIN_CONTRAST = 4.5;

type RGB = [number, number, number];

function hexToRgb(hex: string): RGB | null {
	let h = hex.trim().replace("#", "");
	if (h.length === 3) {
		h = h[0] + h[0] + h[1] + h[1] + h[2] + h[2];
	}
	if (h.length !== 6) return null;
	const n = Number.parseInt(h, 16);
	if (Number.isNaN(n)) return null;
	return [(n >> 16) & 0xff, (n >> 8) & 0xff, n & 0xff];
}

function rgbToHex([r, g, b]: RGB): string {
	const c = (v: number) =>
		Math.round(Math.max(0, Math.min(255, v)))
			.toString(16)
			.padStart(2, "0");
	return `#${c(r)}${c(g)}${c(b)}`;
}

/** Relative luminance per WCAG 2.x. */
function luminance([r, g, b]: RGB): number {
	const chan = (v: number) => {
		const s = v / 255;
		return s <= 0.03928 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
	};
	return 0.2126 * chan(r) + 0.7152 * chan(g) + 0.0722 * chan(b);
}

/** WCAG contrast ratio between two colors (1..21). */
function contrast(a: RGB, b: RGB): number {
	const la = luminance(a);
	const lb = luminance(b);
	const [hi, lo] = la > lb ? [la, lb] : [lb, la];
	return (hi + 0.05) / (lo + 0.05);
}

/** Move `rgb` toward black (factor<1) or white (factor>1 via mix). */
function darken([r, g, b]: RGB, amount: number): RGB {
	return [r * (1 - amount), g * (1 - amount), b * (1 - amount)];
}
function lighten([r, g, b]: RGB, amount: number): RGB {
	return [r + (255 - r) * amount, g + (255 - g) * amount, b + (255 - b) * amount];
}

/**
 * Return an accent hex adjusted so it meets `MIN_CONTRAST` against the theme
 * background. On light mode it darkens the accent; on dark mode it lightens it.
 * If already sufficient, the accent is returned unchanged.
 */
export function contrastSafeAccent(hex: string, mode: ResolvedMode): string {
	const rgb = hexToRgb(hex);
	if (!rgb) return hex;
	const bg = THEME_BG[mode];
	if (contrast(rgb, bg) >= MIN_CONTRAST) {
		return rgbToHex(rgb);
	}
	// Step toward the direction that increases contrast until it passes (or we
	// bottom/top out). Light bg → darken the accent; dark bg → lighten it.
	const adjust = mode === "light" ? darken : lighten;
	let best = rgb;
	for (let step = 1; step <= 20; step++) {
		const candidate = adjust(rgb, step * 0.05);
		best = candidate;
		if (contrast(candidate, bg) >= MIN_CONTRAST) {
			break;
		}
	}
	return rgbToHex(best);
}

/**
 * Resolve a stored accent (swatch id, raw hex, or "") to the `--accent` value
 * for a given theme mode. Swatch ids use the palette's hand-tuned per-theme
 * value; a raw hex is WCAG-adjusted for the mode; "" (or unknown) → null so the
 * base monochrome accent shows.
 */
export function resolveAccent(accent: string, mode: ResolvedMode): string | null {
	const a = accent.trim();
	if (!a) return null;
	const swatch = ACCENTS.find((s) => s.id === a);
	if (swatch) {
		const v = mode === "light" ? swatch.light : swatch.dark;
		if (!v) return null; // default swatch → monochrome
		// The per-theme values are hand-tuned (Radix hues), but Lychi's light bg
		// (#f5f5f5) is slightly darker than Radix's, so a few land just under AA.
		// The contrast net nudges only those the last little bit, preserving hue.
		return contrastSafeAccent(v, mode);
	}
	if (isHexColor(a)) {
		return contrastSafeAccent(a, mode);
	}
	return null;
}

/** The OS's current color-scheme preference, for resolving "system" to a real
 *  mode (accent values are per-theme, so this picks which one to use). */
export function systemMode(): ResolvedMode {
	if (typeof window === "undefined" || !window.matchMedia) return "dark";
	return window.matchMedia("(prefers-color-scheme: light)").matches ? "light" : "dark";
}

/**
 * Apply a theme to the document. Idempotent; safe to call on every change.
 * - `mode` → `data-theme` (drives the light/dark variable set in app.css).
 * - `accent` → sets `--accent` to the per-theme value (never low-contrast/
 *   invisible), or clears the override so the base monochrome accent shows.
 */
export function applyTheme(theme: Theme): void {
	const root = document.documentElement;
	// "system" means: don't stamp a data-theme, so app.css's
	// `@media (prefers-color-scheme)` blocks decide light vs dark (the theme-aware
	// default state). An explicit "dark"/"light" wins via the [data-theme] rules.
	if (theme.mode === "system") {
		delete root.dataset.theme;
	} else {
		root.dataset.theme = theme.mode;
	}

	// Accent values are per-theme, so resolve "system" to whichever the OS is
	// currently showing before picking the swatch's light/dark value.
	const effectiveMode = theme.mode === "system" ? systemMode() : theme.mode;
	const value = resolveAccent(theme.accent, effectiveMode);
	if (value) {
		root.style.setProperty("--accent", value);
	} else {
		root.style.removeProperty("--accent");
	}

	// One choice, both stacks — so a label and the command input never disagree.
	// `--font-output` is intentionally untouched: command output stays
	// fixed-width whatever the user picks.
	applyFont(root, "--font-sans", theme.fontFamily, BASE_SANS);
	applyFont(root, "--font-mono", theme.fontFamily, BASE_MONO);

	// Card opacity: clamp to a readable floor and set as a 0–1 number the card's
	// translucent background reads (see `main` in +page.svelte). 1.0 (the default)
	// removes the override so the card is fully opaque as before.
	const opacity = theme.opacity ?? 1;
	if (opacity < 1) {
		root.style.setProperty("--card-opacity", String(Math.max(MIN_CARD_OPACITY, opacity)));
	} else {
		root.style.removeProperty("--card-opacity");
	}

	// Corner radius: clamp to [0, MAX] and set in px. Undefined / the default 12
	// removes the override so the stylesheet's own radius applies.
	const radius = theme.cornerRadius;
	if (radius != null && radius !== 12) {
		root.style.setProperty(
			"--card-radius",
			`${Math.max(0, Math.min(MAX_CORNER_RADIUS, radius))}px`,
		);
	} else {
		root.style.removeProperty("--card-radius");
	}

	// Frosted-glass CSS fallback tint. Real compositor blur is handled backend-
	// side (KWin); this only drives the visual frost where blur isn't applied.
	if (theme.blur) {
		root.style.setProperty("--card-frost", "1");
	} else {
		root.style.removeProperty("--card-frost");
	}
}

/**
 * The fallback stacks from app.css, repeated here because a CSS custom property
 * can't reference its own previous value — setting `--font-sans` inline
 * replaces the stylesheet's definition outright, so the fallbacks have to be
 * re-stated when prepending a choice.
 *
 * Kept in sync with app.css by the test in theme.test.ts.
 */
const BASE_SANS =
	'system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", "Inter", "Cantarell", "Noto Sans", "Roboto", "DejaVu Sans", sans-serif';
const BASE_MONO =
	'"JetBrains Mono", "Fira Code", "Cascadia Code", "Source Code Pro", "Noto Sans Mono", "DejaVu Sans Mono", "Liberation Mono", ui-monospace, monospace';

/**
 * Put the user's chosen family at the head of a font stack.
 *
 * Prepending rather than replacing is deliberate: if the chosen font is later
 * uninstalled (or the config is copied to another machine that lacks it), the
 * browser walks on to the next entry and the app still renders in something
 * sensible. Replacing the stack outright would drop straight to the generic
 * `monospace`/`sans-serif`, which fontconfig frequently resolves to Bitstream
 * Vera — visibly soft next to any modern face.
 */
function applyFont(
	root: HTMLElement,
	property: string,
	choice: string | undefined,
	base: string,
): void {
	const stack = fontStack(choice, base);
	if (stack) {
		root.style.setProperty(property, stack);
	} else {
		root.style.removeProperty(property);
	}
}

/**
 * The full font stack for a chosen family, or `null` when nothing is chosen
 * (meaning: clear the override and let the stylesheet's stack apply).
 *
 * Separated from the DOM write so the stack-building rules — quoting, escaping,
 * fallback order — are testable without a browser environment.
 */
export function fontStack(choice: string | undefined, base: string): string | null {
	const family = choice?.trim();
	if (!family) return null;
	return `${quoteFamily(family)}, ${base}`;
}

/** The built-in stacks, exported so callers and tests can reference them. */
export const FONT_STACKS = { sans: BASE_SANS, mono: BASE_MONO } as const;

/**
 * Quote a family name for CSS unless it is a bare single identifier.
 *
 * Family names with spaces ("JetBrains Mono") or punctuation ("Anka/Coder")
 * are invalid unquoted, and fontconfig genuinely reports names like the latter.
 */
function quoteFamily(family: string): string {
	if (/^[a-zA-Z][a-zA-Z0-9-]*$/.test(family)) return family;
	// Escape embedded quotes/backslashes so a hostile or odd family name can't
	// terminate the declaration and inject further CSS.
	return `"${family.replace(/[\\"]/g, "\\$&")}"`;
}

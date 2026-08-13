<script lang="ts">
import { Monitor, Moon, RotateCcw, Sun } from "lucide-svelte";
import type { FontFamily, GeneralConfig } from "$lib/ipc";
import { getInstalledFonts, saveGeneralConfig, setCardBlur } from "$lib/ipc";
import {
	ACCENTS,
	MAX_CORNER_RADIUS,
	MIN_CARD_OPACITY,
	resolveAccent,
	systemMode,
	type Theme,
	type ThemeMode,
} from "$lib/theme";
import FontPicker from "./FontPicker.svelte";

let {
	generalConfig = $bindable(),
	onsaveerror,
}: {
	generalConfig: GeneralConfig;
	onsaveerror: (msg: string) => void;
} = $props();

// Installed font families for the picker. Fire-and-forget: enumerating shells
// out to fontconfig, so we don't block the panel's first paint on it.
let fontFamilies = $state<FontFamily[]>([]);
$effect(() => {
	getInstalledFonts()
		.then((f) => {
			fontFamilies = f;
		})
		.catch(() => {
			// No fontconfig — the picker stays empty and the CSS stack applies.
		});
});

// Emit the full theme to the engine (applied live in +layout) + persist. One
// helper for every knob so the applier stays centralized.
async function applyAndSaveTheme() {
	const theme: Theme = {
		mode: (generalConfig.theme as ThemeMode) ?? "dark",
		accent: generalConfig.accent ?? "",
		fontFamily: generalConfig.font_family ?? "",
		opacity: generalConfig.card_opacity ?? 1,
		cornerRadius: generalConfig.corner_radius ?? 12,
	};
	window.dispatchEvent(new CustomEvent<Theme>("lychi-theme-change", { detail: theme }));
	try {
		await saveGeneralConfig(generalConfig);
	} catch (err) {
		console.error("[settings] Failed to save appearance:", err);
		onsaveerror(`Failed to save: ${err}`);
	}
}

async function handleThemeChange(val: string) {
	generalConfig.theme = val;
	await applyAndSaveTheme();
}
async function handleAccentChange(val: string) {
	generalConfig.accent = val;
	await applyAndSaveTheme();
}
async function handleFontChange(val: string) {
	generalConfig.font_family = val;
	await applyAndSaveTheme();
}

// The factory defaults (must match config/schema.rs GeneralConfig::default).
const DEFAULTS = {
	theme: "system",
	accent: "",
	font_family: "",
	card_opacity: 1,
	corner_radius: 12,
	card_blur: false,
} as const;

// Frosted-glass toggle. The backend requests real compositor blur (KWin); as a
// cross-compositor fallback we ALSO drive a CSS "frost" (a subtle tint) via
// --card-frost so the toggle still does something visible where the compositor
// won't blur. Applied both here (live) and by theme.ts on load.
async function toggleBlur() {
	const on = !generalConfig.card_blur;
	generalConfig.card_blur = on;
	document.documentElement.style.setProperty("--card-frost", on ? "1" : "0");
	try {
		await setCardBlur(on);
	} catch (err) {
		console.error("[settings] Failed to toggle blur:", err);
		onsaveerror(`Failed to save: ${err}`);
	}
}
async function resetBlur() {
	if (generalConfig.card_blur) await toggleBlur();
}

// Per-setting reset: a small icon appears beside a field only when it differs
// from its default, and resets that one field. More discoverable + finer-grained
// than a single global reset — you can revert one thing without losing the rest.
const changed = $derived({
	theme: generalConfig.theme !== DEFAULTS.theme && !!generalConfig.theme,
	accent: (generalConfig.accent ?? "") !== DEFAULTS.accent,
	font: (generalConfig.font_family ?? "") !== DEFAULTS.font_family,
	opacity: (generalConfig.card_opacity ?? 1) !== DEFAULTS.card_opacity,
	corners: (generalConfig.corner_radius ?? 12) !== DEFAULTS.corner_radius,
	blur: (generalConfig.card_blur ?? false) !== DEFAULTS.card_blur,
});

async function resetTheme() {
	generalConfig.theme = DEFAULTS.theme;
	await applyAndSaveTheme();
}
async function resetAccent() {
	generalConfig.accent = DEFAULTS.accent;
	await applyAndSaveTheme();
}
async function resetFont() {
	generalConfig.font_family = DEFAULTS.font_family;
	await applyAndSaveTheme();
}
async function resetOpacity() {
	generalConfig.card_opacity = DEFAULTS.card_opacity;
	await applyAndSaveTheme();
}
async function resetCorners() {
	generalConfig.corner_radius = DEFAULTS.corner_radius;
	await applyAndSaveTheme();
}

// Sliders emit continuously while dragging. Apply the live preview on every
// `input` (so the launcher updates as you drag) but only persist on `change`
// (mouse-up), so we don't write config on every intermediate value.
function previewOpacity(v: number) {
	generalConfig.card_opacity = v;
	window.dispatchEvent(
		new CustomEvent<Theme>("lychi-theme-change", {
			detail: {
				mode: (generalConfig.theme as ThemeMode) ?? "dark",
				accent: generalConfig.accent ?? "",
				fontFamily: generalConfig.font_family ?? "",
				opacity: v,
				cornerRadius: generalConfig.corner_radius ?? 12,
			},
		}),
	);
}
function previewRadius(v: number) {
	generalConfig.corner_radius = v;
	window.dispatchEvent(
		new CustomEvent<Theme>("lychi-theme-change", {
			detail: {
				mode: (generalConfig.theme as ThemeMode) ?? "dark",
				accent: generalConfig.accent ?? "",
				fontFamily: generalConfig.font_family ?? "",
				opacity: generalConfig.card_opacity ?? 1,
				cornerRadius: v,
			},
		}),
	);
}

const opacityPct = $derived(Math.round((generalConfig.card_opacity ?? 1) * 100));

// The concrete mode for accent previews: "system" resolves to what the OS shows.
const effectiveMode = $derived(
	generalConfig.theme === "light"
		? "light"
		: generalConfig.theme === "dark"
			? "dark"
			: systemMode(),
);
</script>

<!-- A field label with a reset icon that shows only when the setting differs
     from its default and resets just that one field. -->
{#snippet fieldLabel(text: string, isChanged: boolean, reset: () => void)}
	<span class="field-label">
		{text}
		{#if isChanged}
			<button class="reset-icon" onclick={reset} title="Reset to default" aria-label={`Reset ${text} to default`}>
				<RotateCcw size={11} strokeWidth={2} />
			</button>
		{/if}
	</span>
{/snippet}

<div class="section-label">Theme</div>

<div class="field">
	{@render fieldLabel("Mode", changed.theme, resetTheme)}
	<div class="theme-toggle">
		<button
			class="theme-option"
			class:active={generalConfig.theme === "system" || !generalConfig.theme}
			onclick={() => handleThemeChange("system")}
			title="Follow system"
		>
			<Monitor size={14} />
		</button>
		<button
			class="theme-option"
			class:active={generalConfig.theme === "dark"}
			onclick={() => handleThemeChange("dark")}
			title="Dark"
		>
			<Moon size={14} />
		</button>
		<button
			class="theme-option"
			class:active={generalConfig.theme === "light"}
			onclick={() => handleThemeChange("light")}
			title="Light"
		>
			<Sun size={14} />
		</button>
	</div>
</div>

<div class="field">
	{@render fieldLabel("Accent", changed.accent, resetAccent)}
	<div class="accent-swatches">
		{#each ACCENTS as swatch (swatch.id)}
			{@const isDefault = swatch.id === "default"}
			{@const preview = resolveAccent(swatch.id, effectiveMode)}
			<button
				class="accent-swatch"
				class:active={(generalConfig.accent ?? "") === swatch.id || (isDefault && !generalConfig.accent)}
				class:is-default={isDefault}
				style={preview ? `--swatch: ${preview}` : ""}
				onclick={() => handleAccentChange(swatch.id === "default" ? "" : swatch.id)}
				title={swatch.label}
				aria-label={swatch.label}
				aria-pressed={(generalConfig.accent ?? "") === swatch.id}
			></button>
		{/each}
	</div>
</div>

<div class="field">
	{@render fieldLabel("Font", changed.font, resetFont)}
	<FontPicker value={generalConfig.font_family ?? ""} fonts={fontFamilies} onchange={handleFontChange} />
</div>
<div class="field-hint">
	Applies across the app. Command output stays fixed-width whatever you pick, so
	tables and logs keep their columns.
</div>

<div class="section-label">Launcher card</div>

<div class="field">
	{@render fieldLabel("Opacity", changed.opacity, resetOpacity)}
	<div class="slider-row">
		<input
			id="card-opacity"
			class="slider"
			type="range"
			min={MIN_CARD_OPACITY}
			max="1"
			step="0.01"
			value={generalConfig.card_opacity ?? 1}
			aria-label="Opacity"
			aria-valuetext={`${opacityPct}%`}
			oninput={(e) => previewOpacity(Number((e.currentTarget as HTMLInputElement).value))}
			onchange={applyAndSaveTheme}
		/>
		<span class="slider-value">{opacityPct}%</span>
	</div>
</div>
<div class="field-hint">
	How see-through the launcher background is over your desktop. Clamped so text
	stays readable.
</div>

<div class="field">
	{@render fieldLabel("Corners", changed.corners, resetCorners)}
	<div class="slider-row">
		<input
			id="card-radius"
			class="slider"
			type="range"
			min="0"
			max={MAX_CORNER_RADIUS}
			step="1"
			value={generalConfig.corner_radius ?? 12}
			aria-label="Corners"
			aria-valuetext={`${generalConfig.corner_radius ?? 12}px`}
			oninput={(e) => previewRadius(Number((e.currentTarget as HTMLInputElement).value))}
			onchange={applyAndSaveTheme}
		/>
		<span class="slider-value">{generalConfig.corner_radius ?? 12}px</span>
	</div>
</div>
<div class="field-hint">Corner radius of the launcher window, from square to fully rounded.</div>

<div class="field">
	{@render fieldLabel("Frosted glass", changed.blur, resetBlur)}
	<button
		class="toggle-switch"
		class:on={generalConfig.card_blur}
		onclick={toggleBlur}
		role="switch"
		aria-checked={generalConfig.card_blur}
		aria-label="Frosted glass"
	>
		<span class="knob"></span>
	</button>
</div>
<div class="field-hint">
	Blurs the desktop behind the launcher. Real blur on KDE Plasma; a subtle frost
	tint elsewhere (blur is the compositor's job and not every one supports it).
</div>

<style>
	.field {
		display: flex;
		align-items: center;
		justify-content: space-between;
		padding: 6px 0;
		gap: 12px;
	}

	.field-label {
		color: var(--fg-muted);
		font-size: 12px;
		flex-shrink: 0;
		width: 120px;
		display: inline-flex;
		align-items: center;
	}

	.section-label {
		font-size: 11px;
		color: var(--fg-muted);
		text-transform: uppercase;
		letter-spacing: 0.05em;
		padding: 12px 0 4px;
		border-top: 1px solid var(--border);
		margin-top: 4px;
	}
	/* No top border on the very first section — it sits at the panel top. */
	.section-label:first-child {
		border-top: none;
		margin-top: 0;
		padding-top: 0;
	}

	.field-hint {
		font-size: 10px;
		color: var(--fg-muted);
		opacity: 0.7;
		padding: 0 0 2px;
	}

	.theme-toggle {
		display: flex;
		border: 1px solid var(--border);
		border-radius: 4px;
		overflow: hidden;
	}
	.theme-option {
		background: var(--bg-secondary);
		color: var(--fg-muted);
		border: none;
		padding: 5px 14px;
		font-family: var(--font-mono);
		font-size: 12px;
		cursor: pointer;
		transition: background 100ms ease, color 100ms ease;
		display: flex;
		align-items: center;
		justify-content: center;
	}
	.theme-option:not(:last-child) {
		border-right: 1px solid var(--border);
	}
	.theme-option:hover:not(.active) {
		color: var(--fg);
	}
	.theme-option.active {
		background: var(--border);
		color: var(--fg);
	}

	.accent-swatches {
		display: flex;
		gap: 6px;
		align-items: center;
	}
	.accent-swatch {
		width: 18px;
		height: 18px;
		border-radius: 50%;
		border: 1px solid var(--border);
		background: var(--swatch, transparent);
		cursor: pointer;
		padding: 0;
		transition: transform 100ms ease, box-shadow 100ms ease;
	}
	.accent-swatch.is-default {
		background: linear-gradient(135deg, var(--accent) 50%, var(--fg-muted) 50%);
	}
	.accent-swatch:hover {
		transform: scale(1.15);
	}
	.accent-swatch.active {
		box-shadow: 0 0 0 2px var(--bg-secondary), 0 0 0 3px var(--fg);
	}

	.slider-row {
		display: flex;
		align-items: center;
		gap: 10px;
		flex: 1;
		justify-content: flex-end;
	}
	.slider {
		width: 160px;
		accent-color: var(--accent);
		cursor: pointer;
	}
	.slider-value {
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--fg-muted);
		width: 34px;
		text-align: right;
	}

	/* Per-field reset icon: sits inline after the label, only when the field
	   differs from its default. Quiet until hovered so it never competes with the
	   control it belongs to. */
	.reset-icon {
		background: none;
		border: none;
		padding: 0;
		margin-left: 6px;
		display: inline-flex;
		align-items: center;
		color: var(--fg-muted);
		opacity: 0.6;
		cursor: pointer;
		vertical-align: middle;
		transition: color 100ms ease, opacity 100ms ease;
	}
	.reset-icon:hover {
		color: var(--accent);
		opacity: 1;
	}

	.toggle-switch {
		width: 34px;
		height: 18px;
		border-radius: 9px;
		border: 1px solid var(--border);
		background: var(--bg-secondary);
		padding: 0;
		cursor: pointer;
		position: relative;
		transition: background 120ms ease, border-color 120ms ease;
	}
	.toggle-switch.on {
		background: var(--accent);
		border-color: var(--accent);
	}
	.toggle-switch .knob {
		position: absolute;
		top: 1px;
		left: 1px;
		width: 14px;
		height: 14px;
		border-radius: 50%;
		background: var(--fg);
		transition: transform 120ms ease, background 120ms ease;
	}
	.toggle-switch.on .knob {
		transform: translateX(16px);
		background: var(--bg);
	}
</style>

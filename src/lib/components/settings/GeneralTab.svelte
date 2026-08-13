<script lang="ts">
import { X } from "lucide-svelte";
import type { CommandsConfig, GeneralConfig, HotkeyStatus, PrivacyConfig } from "$lib/ipc";
import {
	getAutostartEnabled,
	getHotkeyStatus,
	getInstalledTerminals,
	recordHotkey,
	restartApp,
	saveCommandsConfig,
	saveGeneralConfig,
	savePrivacyConfig,
	setAutostartEnabled,
	setHotkey,
} from "$lib/ipc";
import Select from "../Select.svelte";

let {
	generalConfig = $bindable(),
	commandsConfig = $bindable(),
	privacyConfig = $bindable(),
	layerShellSupported,
	activeWindowStrategy,
	screenComposited = true,
	onsaveerror,
}: {
	generalConfig: GeneralConfig;
	commandsConfig: CommandsConfig;
	privacyConfig: PrivacyConfig;
	layerShellSupported: boolean;
	activeWindowStrategy: string;
	screenComposited?: boolean;
	onsaveerror: (msg: string) => void;
} = $props();

let recordingHotkey = $state(false);
let hotkeyError = $state("");
let customShell = $state(false);
let terminalOptions: { value: string; label: string }[] = $state([]);
let hotkeyStatus = $state<HotkeyStatus | null>(null);
let autostartEnabled = $state(false);

// Fetch hotkey/session status and autostart state on mount (non-blocking)
getHotkeyStatus().then((s) => {
	hotkeyStatus = s;
});
getAutostartEnabled()
	.then((v) => {
		autostartEnabled = v;
	})
	.catch(() => {});

async function handleAutostartToggle() {
	const next = !autostartEnabled;
	autostartEnabled = next;
	try {
		await setAutostartEnabled(next);
	} catch (err) {
		autostartEnabled = !next;
		console.error("[settings] Failed to toggle autostart:", err);
		onsaveerror(`Failed to update autostart: ${err}`);
	}
}

let isWayland = $derived(hotkeyStatus?.session_type === "wayland");

// Fetch installed terminals on mount (non-blocking)
getInstalledTerminals().then((terminals) => {
	terminalOptions = terminals.map((t) => ({ value: t, label: t }));
	// If current terminal isn't in the list, add it so the dropdown shows it
	if (commandsConfig.terminal && !terminals.includes(commandsConfig.terminal)) {
		terminalOptions.unshift({ value: commandsConfig.terminal, label: commandsConfig.terminal });
	}
});

const knownShells = [
	"/bin/bash",
	"/bin/zsh",
	"/bin/fish",
	"/bin/sh",
	"/usr/bin/nu",
	"/usr/bin/pwsh",
];

let shellOptions = $derived([
	{ value: "/bin/bash", label: "Bash" },
	{ value: "/bin/zsh", label: "Zsh" },
	{ value: "/bin/fish", label: "Fish" },
	{ value: "/bin/sh", label: "sh" },
	{ value: "/usr/bin/nu", label: "Nushell" },
	{ value: "/usr/bin/pwsh", label: "PowerShell" },
	{ value: "__custom__", label: "Custom..." },
]);

// Mirrors resolve_strategy() in platform/linux.rs
function resolveStrategy(strategy: string): string {
	if (strategy === "layer-shell")
		return layerShellSupported ? "layer-shell" : isWayland ? "toplevel" : "x11";
	if (strategy === "toplevel" || strategy === "toplevel-window") return "toplevel";
	if (strategy === "x11") return "x11";
	// auto
	if (!hotkeyStatus) return activeWindowStrategy; // status not loaded yet
	if (isWayland && hotkeyStatus.desktop.toUpperCase().includes("KDE")) return "toplevel";
	if (layerShellSupported) return "layer-shell";
	return isWayland ? "toplevel" : "x11";
}

let strategyNeedsRestart = $derived(
	resolveStrategy(generalConfig.window_strategy) !== activeWindowStrategy,
);

export function initCustomShell(shell: string) {
	customShell = !knownShells.includes(shell);
}

async function handleHideOnBlurToggle() {
	generalConfig.hide_on_blur = !generalConfig.hide_on_blur;
	try {
		await saveGeneralConfig(generalConfig);
	} catch (err) {
		console.error("[settings] Failed to save general config:", err);
		onsaveerror(`Failed to save: ${err}`);
	}
}

async function handleMonitorModeChange(val: string) {
	generalConfig.monitor_mode = val;
	try {
		await saveGeneralConfig(generalConfig);
	} catch (err) {
		console.error("[settings] Failed to save monitor mode:", err);
		onsaveerror(`Failed to save: ${err}`);
	}
}

async function handleWindowStrategyChange(val: string) {
	generalConfig.window_strategy = val;
	try {
		await saveGeneralConfig(generalConfig);
	} catch (err) {
		console.error("[settings] Failed to save window strategy:", err);
		onsaveerror(`Failed to save: ${err}`);
	}
}

function handleShellSelect(val: string) {
	if (val === "__custom__") {
		customShell = true;
		commandsConfig.shell = "";
		return;
	}
	handleShellChange(val);
}

async function handleShellChange(val: string) {
	if (!val.trim()) return;
	commandsConfig.shell = val.trim();
	try {
		await saveCommandsConfig(commandsConfig);
	} catch (err) {
		console.error("[settings] Failed to save shell config:", err);
		onsaveerror(`Failed to save: ${err}`);
	}
}

async function handleTerminalChange(val: string) {
	if (!val.trim()) return;
	commandsConfig.terminal = val.trim();
	try {
		await saveCommandsConfig(commandsConfig);
	} catch (err) {
		console.error("[settings] Failed to save terminal config:", err);
		onsaveerror(`Failed to save: ${err}`);
	}
}
</script>

<div class="field">
	<span class="field-label">Hotkey</span>
	<div class="hotkey-row">
		<button
			class="hotkey-btn"
			class:recording={recordingHotkey}
			onclick={async () => {
				if (recordingHotkey) return;
				recordingHotkey = true;
				hotkeyError = "";
				try {
					const combo = await recordHotkey();
					await setHotkey(combo);
					generalConfig.hotkey = combo;
				} catch (err) {
					const msg = String(err);
					if (!msg.includes("Cancelled")) {
						hotkeyError = msg;
					}
				} finally {
					recordingHotkey = false;
				}
			}}
		>
			{#if recordingHotkey}
				Press keys...
			{:else}
				{generalConfig.hotkey}
			{/if}
		</button>
	</div>
</div>
{#if hotkeyError}
	<div class="field-error">{hotkeyError}</div>
{/if}
{#if hotkeyStatus && !hotkeyStatus.reliable}
	<div class="field-hint">
		<!-- The backend already decided why this is unreliable; restating that
		     judgement from `isWayland` here is how the two answers drifted apart. -->
		{hotkeyStatus.explanation}.
		{#if hotkeyStatus.needs_confirmation}
			Press it to check — this notice clears once a press reaches Lychi.
		{:else}
			Bind <code>lychi --toggle</code> to a key in your desktop's shortcut settings.
		{/if}
	</div>
{/if}
<div class="field">
	<label for="autostart">Start at login</label>
	<button
		id="autostart"
		class="checkbox"
		class:checked={autostartEnabled}
		onclick={handleAutostartToggle}
		role="checkbox"
		aria-checked={autostartEnabled}
	>
		{#if autostartEnabled}
			<svg width="12" height="12" viewBox="0 0 12 12" fill="none">
				<path d="M2 6L5 9L10 3" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"/>
			</svg>
		{/if}
	</button>
</div>
<div class="field-hint">Launches hidden in the background — summon with the hotkey or <code>lychi --toggle</code></div>
<div class="field">
	<label for="hide-blur">Hide on blur</label>
	<button
		id="hide-blur"
		class="checkbox"
		class:checked={generalConfig.hide_on_blur}
		onclick={handleHideOnBlurToggle}
		role="checkbox"
		aria-checked={generalConfig.hide_on_blur}
	>
		{#if generalConfig.hide_on_blur}
			<svg width="12" height="12" viewBox="0 0 12 12" fill="none">
				<path d="M2 6L5 9L10 3" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"/>
			</svg>
		{/if}
	</button>
</div>
<div class="field">
	<label for="monitor-mode">Open on</label>
	<Select
		id="monitor-mode"
		value={generalConfig.monitor_mode}
		options={[
			{ value: "cursor", label: "Current monitor" },
			{ value: "primary", label: "Primary monitor" },
		]}
		onchange={handleMonitorModeChange}
	/>
</div>
<div class="field">
	<label for="window-strategy">Window strategy</label>
	<Select
		id="window-strategy"
		value={generalConfig.window_strategy}
		options={[
			{ value: "auto", label: "Auto (recommended)" },
			...(layerShellSupported
				? [{ value: "layer-shell", label: "Layer shell (Wayland)" }]
				: []),
			...(isWayland || activeWindowStrategy === "toplevel" || layerShellSupported
				? [{ value: "toplevel", label: "Toplevel window (Wayland)" }]
				: []),
			...(isWayland && !layerShellSupported
				? [{ value: "toplevel-window", label: "Toplevel, no fullscreen (troubleshooting)" }]
				: []),
			{ value: "x11", label: "X11 fullscreen overlay" },
		]}
		onchange={handleWindowStrategyChange}
	/>
	{#if strategyNeedsRestart}
		<button class="restart-btn" onclick={() => restartApp()}>
			Restart to apply
		</button>
	{/if}
</div>
{#if !screenComposited}
	<div class="field-error">
		Your window manager has compositing disabled — the launcher background will
		render opaque. Enable compositing (XFCE: Window Manager Tweaks → Compositor;
		MATE: marco compositing) for transparency.
	</div>
{/if}
<div class="field">
	<label for="shell-select">Shell</label>
	{#if customShell}
		<div class="key-row">
			<input
				id="shell-input"
				type="text"
				bind:value={commandsConfig.shell}
				spellcheck="false"
				placeholder="/path/to/shell"
				onkeydown={(e) => { if (e.key === "Enter") { e.preventDefault(); handleShellChange(commandsConfig.shell); } }}
			/>
			<button class="set-btn" onclick={() => handleShellChange(commandsConfig.shell)}>Set</button>
			<button class="set-btn" onclick={() => { customShell = false; commandsConfig.shell = "/bin/bash"; handleShellChange("/bin/bash"); }} title="Back to list">
				<X size={12} strokeWidth={2} />
			</button>
		</div>
	{:else}
		<Select
			id="shell-select"
			value={commandsConfig.shell}
			options={shellOptions}
			onchange={handleShellSelect}
		/>
	{/if}
</div>
<div class="field">
	<label for="terminal-select">Terminal</label>
	{#if terminalOptions.length > 0}
		<Select
			id="terminal-select"
			value={commandsConfig.terminal}
			options={terminalOptions}
			onchange={handleTerminalChange}
		/>
	{:else}
		<Select
			id="terminal-select"
			value={commandsConfig.terminal}
			options={[{ value: commandsConfig.terminal, label: commandsConfig.terminal || "detecting..." }]}
			onchange={handleTerminalChange}
		/>
	{/if}
</div>
<div class="section-label">Privacy</div>
<div class="field">
	<label for="allow-geolocation">Allow IP geolocation</label>
	<button
		id="allow-geolocation"
		class="checkbox"
		class:checked={privacyConfig.allow_ip_geolocation}
		onclick={async () => {
			privacyConfig.allow_ip_geolocation = !privacyConfig.allow_ip_geolocation;
			await savePrivacyConfig(privacyConfig);
		}}
		role="checkbox"
		aria-checked={privacyConfig.allow_ip_geolocation}
	>
		{#if privacyConfig.allow_ip_geolocation}
			<svg width="12" height="12" viewBox="0 0 12 12" fill="none">
				<path d="M2 6L5 9L10 3" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"/>
			</svg>
		{/if}
	</button>
</div>
<div class="field-hint">Weather auto-detect via freeipapi.com</div>
<div class="field">
	<label for="allow-public-ip">Allow public IP lookup</label>
	<button
		id="allow-public-ip"
		class="checkbox"
		class:checked={privacyConfig.allow_public_ip}
		onclick={async () => {
			privacyConfig.allow_public_ip = !privacyConfig.allow_public_ip;
			await savePrivacyConfig(privacyConfig);
		}}
		role="checkbox"
		aria-checked={privacyConfig.allow_public_ip}
	>
		{#if privacyConfig.allow_public_ip}
			<svg width="12" height="12" viewBox="0 0 12 12" fill="none">
				<path d="M2 6L5 9L10 3" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"/>
			</svg>
		{/if}
	</button>
</div>
<div class="field-hint">sysinfo net/ip via ifconfig.me</div>
<div class="field">
	<label for="clipboard-sensitive">Skip passwords in clipboard history</label>
	<button
		id="clipboard-sensitive"
		class="checkbox"
		class:checked={privacyConfig.clipboard.respect_sensitive_hint}
		onclick={async () => {
			privacyConfig.clipboard.respect_sensitive_hint =
				!privacyConfig.clipboard.respect_sensitive_hint;
			await savePrivacyConfig(privacyConfig);
		}}
		role="checkbox"
		aria-checked={privacyConfig.clipboard.respect_sensitive_hint}
	>
		{#if privacyConfig.clipboard.respect_sensitive_hint}
			<svg width="12" height="12" viewBox="0 0 12 12" fill="none">
				<path d="M2 6L5 9L10 3" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"/>
			</svg>
		{/if}
	</button>
</div>
<div class="field-hint">
	Don't record copies that a password manager marked secret. Only works for apps
	that set the marker.
</div>

<style>
	.field {
		display: flex;
		align-items: center;
		justify-content: space-between;
		padding: 6px 0;
		gap: 12px;
	}

	label,
	.field-label {
		color: var(--fg-muted);
		font-size: 12px;
		flex-shrink: 0;
		width: 120px;
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

	.field-hint {
		font-size: 10px;
		color: var(--fg-muted);
		opacity: 0.7;
		padding: 0 0 2px;
	}

	.field-hint code {
		font-family: var(--font-mono);
		background: var(--bg-secondary);
		padding: 1px 4px;
		border-radius: 3px;
		user-select: all;
	}

	.field-error {
		font-size: 11px;
		color: var(--error);
		padding: 2px 0 4px 0;
	}

	.restart-btn {
		font-size: 11px;
		padding: 2px 8px;
		border-radius: 4px;
		border: 1px solid var(--warning, #f59e0b);
		background: transparent;
		color: var(--warning, #f59e0b);
		cursor: pointer;
		flex-shrink: 0;
	}

	.restart-btn:hover {
		background: var(--warning, #f59e0b);
		color: var(--bg);
	}

	input[type="text"] {
		background: var(--bg-secondary);
		color: var(--fg);
		border: 1px solid var(--border);
		border-radius: 4px;
		padding: 5px 8px;
		font-family: var(--font-mono);
		font-size: 12px;
		outline: none;
		flex: 1;
		min-width: 0;
	}

	input[type="text"]:focus {
		border-color: var(--fg-muted);
	}

	.key-row {
		display: flex;
		gap: 6px;
		flex: 1;
		min-width: 0;
		align-items: center;
	}

	.key-row input {
		flex: 1;
		min-width: 0;
	}

	.set-btn {
		background: var(--bg-secondary);
		color: var(--accent);
		border: 1px solid var(--border);
		border-radius: 4px;
		padding: 5px 12px;
		font-family: var(--font-mono);
		font-size: 12px;
		cursor: pointer;
		flex-shrink: 0;
		transition: background 100ms ease;
	}

	.set-btn:hover:not(:disabled) {
		background: var(--border);
	}

	.set-btn:disabled {
		opacity: 0.4;
		cursor: default;
	}

	.hotkey-row {
		display: flex;
		gap: 6px;
		align-items: center;
	}

	.hotkey-btn {
		background: var(--bg-secondary);
		color: var(--fg);
		border: 1px solid var(--border);
		border-radius: 4px;
		padding: 5px 12px;
		font-family: var(--font-mono);
		font-size: 12px;
		min-width: 140px;
		text-align: center;
		cursor: pointer;
		transition: border-color 100ms ease;
	}

	.hotkey-btn:hover {
		border-color: var(--fg-muted);
	}

	.hotkey-btn.recording {
		border-color: var(--accent);
		color: var(--accent);
		animation: hotkey-pulse 1s ease-in-out infinite;
	}

	@keyframes hotkey-pulse {
		0%, 100% { opacity: 1; }
		50% { opacity: 0.4; }
	}


	.checkbox {
		width: 18px;
		height: 18px;
		display: flex;
		align-items: center;
		justify-content: center;
		background: var(--bg-secondary);
		border: 1px solid var(--border);
		border-radius: 3px;
		color: var(--accent);
		cursor: pointer;
		padding: 0;
		flex-shrink: 0;
		transition: border-color 100ms ease;
	}

	.checkbox:hover {
		border-color: var(--fg-muted);
	}

	.checkbox.checked {
		background: var(--bg-secondary);
		border-color: var(--fg-muted);
	}
</style>

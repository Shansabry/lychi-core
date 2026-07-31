/**
 * Send frontend failures to the backend log file.
 *
 * The two halves of the app used to report to different places: Rust wrote to
 * `~/.local/share/lychi/logs`, while the webview's `console.error` went to a
 * devtools console nobody has open. So a frontend crash produced a log that
 * looked *healthy* — every warmup succeeded, IPC was fine, no errors — and the
 * only symptom was a window that stopped responding.
 *
 * A tester hit exactly that: "it crashes when I type about three characters",
 * with a log containing nothing but a clean startup. He worked it out himself —
 * "the logs are only for the backend, the thing that's crashing is the
 * frontend" — which is the gap this closes.
 *
 * Not `tauri-plugin-log`, deliberately. It does not capture uncaught errors
 * (no `window.onerror`, no `unhandledrejection` — its own docs say console
 * forwarding is a shim you write), so it would not have caught this bug either,
 * and adopting it would mean a SECOND log file for a user to find and send.
 * The `forwardConsole` pattern below is taken from those docs; only the
 * transport differs — straight into the logger the rest of the app already uses.
 */

import { commands } from "./bindings";

/** Fire-and-forget: a logging call that can throw is a logging call that ends up in a try/catch and gets dropped. */
function send(level: "error" | "warn" | "info", message: string, stack?: string): void {
	void commands.logFrontend({ level, message, stack: stack ?? null }).catch(() => {});
}

/**
 * Deliberately record a frontend event in the backend log.
 *
 * Distinct from the `console.*` mirroring below, which catches things that went
 * wrong. This is for tracing a flow that is *working* but whose decisions are
 * invisible from the Rust side — the blur-dismiss path being the case that
 * motivated it: the backend logs that it asked for a dismiss, and only the
 * frontend knows whether it honoured it, declined it, or did something else.
 */
export const uiLog = {
	info: (message: string) => send("info", message),
	warn: (message: string) => send("warn", message),
	error: (message: string) => send("error", message),
};

/** Best-effort stringify — an Error, a DOM event, or whatever a library threw. */
function describe(value: unknown): { message: string; stack?: string } {
	if (value instanceof Error) {
		return { message: `${value.name}: ${value.message}`, stack: value.stack };
	}
	if (typeof value === "string") return { message: value };
	try {
		return { message: JSON.stringify(value) };
	} catch {
		return { message: String(value) };
	}
}

/**
 * Guards against installing twice.
 *
 * `onMount` re-runs on every hot reload, and each run would add another pair of
 * listeners and wrap `console.error` around the previous wrapper — so one error
 * arrives 2×, then 3×, compounding with each edit. Observed immediately: a
 * self-test error logged twice after a single HMR update. The same applies to
 * any genuine remount, so this is not a dev-only concern.
 */
let installed = false;

/**
 * Install the global handlers. Call once, as early as possible — an error thrown
 * before this runs is still invisible. Safe to call again; later calls no-op.
 */
export function installUiLogging(): void {
	if (installed) return;
	installed = true;
	// Uncaught exceptions. `error` carries the real stack; `message`/`source` are
	// the fallback when a cross-origin script strips it.
	window.addEventListener("error", (e: ErrorEvent) => {
		const { message, stack } = describe(e.error ?? e.message);
		send("error", `uncaught: ${message} (${e.filename}:${e.lineno}:${e.colno})`, stack);
	});

	// Rejected promises with no `.catch`. This is the one that matters for an
	// IPC-heavy UI: an `invoke` that rejects inside an event handler surfaces
	// here and nowhere else.
	window.addEventListener("unhandledrejection", (e: PromiseRejectionEvent) => {
		const { message, stack } = describe(e.reason);
		send("error", `unhandled rejection: ${message}`, stack);
	});

	// Mirror console.error/warn without silencing them — devtools still works in
	// development, and the file gets a copy for the reports that matter.
	for (const [name, level] of [
		["error", "error"],
		["warn", "warn"],
	] as const) {
		const original = console[name];
		console[name] = (...args: unknown[]) => {
			original(...args);
			send(level, args.map((a) => describe(a).message).join(" "));
		};
	}

	// A positive "the bridge is alive" marker.
	//
	// Without it, silence is ambiguous: a log with no `[ui]` lines could mean the
	// frontend is healthy, or that this never installed and frontend errors are
	// being dropped exactly as before. One line at startup makes the difference
	// visible in the same log a user pastes — if you see this and nothing else,
	// the frontend really was fine.
	send("info", `frontend logging active (${navigator.userAgent})`);
}

<div align="center">

# 🚀 Lychi

**A local-first command launcher for Linux.**
Spotlight/Raycast energy — keyboard-driven, privacy-first, AI optional.

Open it with `Super+Space`, type what you want, hit Enter. That's it.

`open firefox` · `web rust lang` · `run ls -la` · `calc 12% of 340` · `screenshot area` · `media pause all`

Built with **Tauri v2** (Rust) + **Svelte 5**, shipped as a single **AppImage**. No telemetry. No account. Your data never leaves the machine unless *you* wire up an AI key.

</div>

---

## ✨ Why Lychi

- **Local-first, always.** Everything runs on your box. AI is opt-in — bring your own key, run Ollama, or use the bundled offline model. Off by default.
- **Keyboard is the interface.** ~45 built-in commands, trigger characters, fuzzy everything. Your hands never leave the home row.
- **One launcher, every desktop.** GNOME, KDE, Hyprland, Sway, XFCE, X11 — Lychi picks the right window strategy for your session automatically.
- **It feels like yours.** Frecency-ranked results, your recent apps, your scripts, your quicklinks. Not a list of somebody else's defaults.
- **Genuinely fast.** Cold-start tuned, icon warmup measured in milliseconds, no jank on first render.

---

## 📦 Install

Lychi ships as a self-contained **AppImage** — no package manager, no root, no dependencies to chase.

### 1. Get the AppImage

Download the latest `Lychi_*.AppImage` from [**Releases**](https://github.com/Shansabry/lychi-core/releases), or [build it yourself](#-building-from-source).

### 2. Put it somewhere permanent

```bash
mkdir -p ~/Applications
mv ~/Downloads/Lychi_*.AppImage ~/Applications/Lychi.AppImage
chmod +x ~/Applications/Lychi.AppImage
```

> Keep it at a **stable path** — the desktop entry and the `lychi` CLI both point at wherever it lives, so moving it later means updating those. `~/Applications/Lychi.AppImage` is a good home.

### 3. Run it

```bash
~/Applications/Lychi.AppImage
```

First launch registers the **`Super+Space`** hotkey (your compositor may ask you to confirm) and drops a tray icon. Press the hotkey — the launcher appears.

### 4. Finish setup inside Lychi *(recommended)*

Open Lychi → **Settings → Setup**. That page is your diagnostics + install helper:

- **Install the `lychi` CLI** — one click creates a symlink on your `PATH` so `lychi --toggle`, `lychi --screenshot`, and `lychi --ai` work from anywhere and from desktop shortcuts. (It tracks the AppImage, so re-run it if you ever move or replace the file.)
- **Hotkey status** — Setup tells you exactly which binding route worked (portal / DE settings / X11 grab) and flags it if `Super+Space` was already taken, so you know your hotkey is live.
- **Diagnostics** — a copy-paste report of your desktop, session type, and what Lychi can use on it. This is the single most useful thing to attach to a bug report.

Nothing on this page is ever marked "done" permanently — replacing the AppImage dangles the CLI link, switching X11↔Wayland can break a working hotkey — so it re-checks every time and only offers a fix when one is actually needed.

The **app-menu entry** (and login autostart) is the one manual bit — see below. No config file to write, though; every setting has a sensible default.

<details>
<summary><b>Prefer to wire it up by hand?</b></summary>

**Desktop entry** — `~/.local/share/applications/lychi.desktop`:

```ini
[Desktop Entry]
Type=Application
Name=Lychi
Comment=Local-first keyboard command launcher
Exec=/home/YOU/Applications/Lychi.AppImage %u
Icon=lychi-app
Terminal=false
Categories=Utility;
StartupWMClass=lychi-app
MimeType=x-scheme-handler/lychi;
```

Then `update-desktop-database ~/.local/share/applications`.

**Bind the hotkey yourself** (if the auto-registration didn't take) — point your desktop's keyboard settings at:

```bash
lychi --toggle
```

</details>

---

## ⌨️ Using Lychi

Type a command, or just describe what you want (with AI enabled). A few favourites:

| Command | Example | Does |
|---------|---------|------|
| `open` | `open firefox` | Launch an app (XDG `.desktop` discovery) |
| `web` | `web rust lang` | Search the web (custom engines + bangs) |
| `run` | `run ls -la` | Run a shell command (gated by the rules engine) |
| `calc` | `calc 2+2` | Math, units, currency |
| `file` | `/ report.pdf` | Fuzzy file search — open, reveal, drill in |
| `project` | `project lychi` | Open a project in your editor |
| `media` | `media pause all` | Control any MPRIS player (Spotify, browsers, VLC) |
| `clip` | `cl: token` | Clipboard history |
| `note` / `todo` | `note pick up milk` | Quick notes & todos |
| `screenshot` | `screenshot area` | Full screen, region, or window |
| `packages` | `packages install ripgrep` | dnf · apt · pacman · zypper · flatpak |
| `service` | `service restart nginx` | Control systemd services |
| `win` | `win code` | Switch between open windows |
| `generate` | `generate uuid` | Passwords, UUIDs, tokens |
| `color` | `#3b82f6` | Convert hex/RGB/HSL, nearest Tailwind |

### Trigger characters

| Type… | …and Lychi does |
|-------|-----------------|
| `=` | Math / unit / currency |
| `>` | Shell command |
| `/` | Fuzzy file search |
| `~/` | Open a path from home |
| `@` | Reference a file inside a command |
| `#hex` | Preview a colour |
| `example.com` | Open a URL |

Two-letter colon prefixes jump straight to a handler: `bm:`, `cl:`, `si:`, `yt:`, `e:`, `w:`, `tm:`, `sn:`, and more. With AI on, plain-English input routes itself. **Settings → Guide** lists every command and trigger, generated live from the registry.

### Keyboard shortcuts

All rebindable under `[keybindings]` in `config.toml`. Defaults:

| Shortcut | Action |
|----------|--------|
| `Super+Space` | Show / hide the launcher |
| `Ctrl+K` | Action panel for the selected result |
| `Enter` | Submit |
| `Escape` | Dismiss / close panel |
| `Tab` / `Shift+Tab` | Accept completion / step back |
| `Ctrl+Enter` | Search the web with the current input |
| `Shift+Enter` | Run in a new terminal |
| `Ctrl+1…4` | Toggle history / notes / media / settings panels |
| `Ctrl+Shift+C` | Copy path |
| `Ctrl+Shift+S` | Screenshot |
| `Ctrl+Shift+A` | Attach file |

### AI (optional)

Off by default. Turn it on in **Settings → AI** and pick your lane:

| Mode | What it is |
|------|-----------|
| `byo` | Bring your own key — Anthropic, OpenAI, Groq, Grok, Gemini, OpenRouter, or a custom endpoint |
| `ollama` | Point at a local Ollama server |
| `local` | A bundled llama.cpp model, downloaded on demand — fully offline |

With AI on, natural-language input is routed to the right command, and the agent can chain tools to get things done. It **suggests and acts through the same permission gate as everything else** — destructive steps pause for your approval. API keys live in the system keyring, never in a file.

---

## 🖥️ Desktop Environment Support

Lychi picks the best window strategy for your session automatically:

| Session | Strategy |
|---------|----------|
| wlroots compositors (Hyprland, Sway, …) | wlr-layer-shell overlay |
| KDE Plasma Wayland | toplevel window (KWin layer-shell focus is unreliable) |
| GNOME Wayland | monitor-covering transparent toplevel (Mutter has no layer-shell) |
| X11 (KDE, XFCE, Cinnamon, MATE, …) | fullscreen overlay, or a compact opaque window when compositing is off |

On GNOME the window covers the monitor with a transparent surface and the launcher is centered by CSS — Mutter does not let applications position their own windows. True fullscreen is deliberately **not** requested there, because Mutter paints an opaque backdrop behind fullscreen windows.

**Global hotkey:** Lychi registers `Super+Space` for you where it can, by three routes in order of preference:

| Session | How |
|---------|-----|
| GNOME, KDE (Wayland) | XDG GlobalShortcuts portal — the compositor asks you to confirm |
| XFCE, GNOME, Cinnamon (X11) | written into the desktop's own keyboard settings |
| Everything else | X11 key-grab, or bind `lychi --toggle` yourself |

An X11 key-grab alone is not enough: it cannot override a combination the window manager already owns, and it fails *silently* — which is why the desktop's own settings are written too. If the key you picked is already taken, Lychi says so in the log and leaves the existing binding alone rather than stealing it; pick another in Settings, or bind `lychi --toggle` manually.

---

## ⚙️ CLI

```bash
lychi start                      # start Lychi (no-op if already running)
lychi --toggle                   # show/hide the launcher
lychi --screenshot [area|window] # capture without opening the launcher
lychi --ai [preset]              # run an AI preset on the selected text
lychi --help
```

Every verb except `start` talks to a running instance over a Unix socket, so they're cheap to bind to desktop shortcuts.

---

## 🗂️ Config & Data

Config lives at `~/.config/lychi/config.toml`. Every field has a default — a missing or empty file is valid, and unknown keys merge over the defaults rather than being rejected.

```toml
[general]
hide_on_blur = true
theme = "dark"

[commands]
default_search_engine = "https://www.google.com/search?q="
shell = "/bin/sh"

[history]
max_entries = 500
deduplicate = true

[ai]
mode = "disabled"        # "disabled" | "byo" | "ollama" | "local"
provider = "anthropic"   # BYO preset: anthropic, openai, groq, grok, gemini, openrouter, custom
model = "claude-sonnet-4-5-20250929"   # always user-typed — no baked-in model lists
base_url = ""            # override the preset's endpoint (https required, except loopback)
wire_format = ""         # "openai" | "anthropic" | "gemini" — inferred from the preset if empty
ollama_url = "http://localhost:11434"
local_model = ""         # bundled llama.cpp model, downloaded on demand
```

API keys are stored in the system keyring, never in `config.toml`.

| Path | Purpose |
|------|---------|
| `~/.config/lychi/config.toml` | App configuration |
| `~/.config/lychi/scripts/` | Script Commands (any executable becomes a command) |
| `~/.local/share/lychi/lychi.redb` | History, notes, todos, clipboard, snippets, aliases, reminders, timers, AI presets & chats |
| `~/.local/share/lychi/models/` | Downloaded local AI weights |
| `~/.local/share/lychi/clipboard-images/` | Clipboard image store |
| `~/.local/share/lychi/logs/` | Rotating JSON logs |

---

## 🧪 Testing & feedback

Lychi is pre-1.0 and solo-maintained. The single most useful thing you can do is **run it on your setup and say what happened** — especially if your desktop, compositor, or distro isn't a common one.

Lychi aims to behave identically on every Linux desktop, and window management is where they differ most: placement, focus, transparency, and dismiss behaviour are all compositor-dependent. Bugs there are close to impossible to find without reports from the desktop they happen on.

Useful things to report:

- The launcher appears in the wrong place, at the wrong size, or covers the screen
- It won't take keyboard focus, or won't dismiss when it should
- A command didn't do what its name suggests
- You opened it, didn't know what to type, and closed it again — genuinely useful, and hard to see from the inside

[**Open an issue**](https://github.com/Shansabry/lychi-core/issues) — the bug form asks for your distro, desktop, and session type, which is usually enough to reproduce. Running Lychi from a terminal and pasting the first ~20 lines of output helps too; those lines show which window strategy was picked.

---

# 🛠️ Developer Guide

Everything below is for hacking on Lychi. If you just want to *use* it, you're already done above.

## Building from source

### Prerequisites

- **Rust** >= 1.93.0 (edition 2024)
- **Node.js** >= 20
- **pnpm** >= 10
- **Tauri CLI** (`cargo install tauri-cli`)
- **Linux system dependencies** (Fedora):
  ```bash
  sudo dnf install gtk3-devel webkit2gtk4.1-devel libsoup3-devel \
    librsvg2-devel pango-devel gdk-pixbuf2-devel \
    libappindicator-gtk3-devel
  ```

### Setup

```bash
git clone https://github.com/Shansabry/lychi-core.git
cd lychi-core
pnpm install
rustc --version          # should be >= 1.93.0
cargo tauri --version
```

### Build the AppImage

```bash
pnpm build
```

This builds the CLI binary, runs `tauri build` with the `local-ai` feature and `NO_STRIP=1`, then post-processes the AppImage with `scripts/fix-appimage-codecs.sh`. The output lands in **`target/release/bundle/appimage/`** (a Cargo workspace shares one `target/` at the root).

> **The post-build step is not optional.** Tauri's linuxdeploy bundles WebKitGTK's entire transitive GStreamer stack, whose ELF constructors crash the dynamic loader at `_dl_init` on hosts with a newer glibc than the build box. `fix-appimage-codecs.sh` keeps only the `dlopen`'d GTK plugins (input-method, pixbuf, print), drops the shared libraries, and repacks — which also takes the AppImage from ~114 MB to ~16 MB. Anything it keeps or drops is an explicit policy in that file, deliberately **not** a scan of the build machine.

Once built, install it exactly as in [Install](#-install) above — point it at `target/release/bundle/appimage/Lychi_*.AppImage`.

## Development

```bash
pnpm dev            # launch with hot reload (frontend + backend)
pnpm dev:local-ai   # same, with the bundled local AI compiled in (slower first build)
pnpm test           # all Rust tests
pnpm test:core      # core library tests only
pnpm test:fe        # frontend tests (Vitest)
```

## Code quality

```bash
# Rust
pnpm format:rust    # cargo fmt --all
pnpm clippy         # cargo clippy --workspace
cargo check         # type check all crates

# TypeScript / Svelte
pnpm lint           # Biome lint
pnpm lint:fix       # Biome lint + auto-fix
pnpm format         # Biome format
pnpm check          # svelte-check
```

## Architecture

- **`lychi-core`** — All business logic. Zero Tauri knowledge. Testable in isolation. Organised as LEGO bricks: `action_registry`, `rules`, `intent`, `executor`, `coordinator`, `providers`.
- **`src-tauri`** — Thin Tauri shell. Bridges core to frontend via IPC commands (5–10 lines each).
- **`cli`** — Tiny binary that talks to the running app over a Unix domain socket.

Commands are extensible via the `ActionHandler` trait + `ActionRegistry` dispatch. Adding a new command = 1 new handler file + 1 registration line in `state.rs`; no frontend changes needed.

Every execution passes through the rules engine, which owns the shell/path/URI permission decisions in one place. AI suggests actions but never executes them directly.

### Project structure

```
core/
├── Cargo.toml                     # Workspace root (3 crates)
├── crates/lychi-core/             # Core library — all business logic
│   └── src/
│       ├── action_registry/       # ActionHandler trait + handler implementations
│       │   └── handlers/          # ~45 handlers: open, web, run, calc, file, media, …
│       ├── intent/                # Intent resolution (patterns, classifier, AI routing)
│       ├── rules/                 # Permission deciders (shell, path, uri) + risk levels
│       ├── executor/              # Executor pipeline: resolve → validate → execute
│       ├── coordinator/           # Cross-brick orchestration + the AI tool-calling loop
│       ├── providers/             # AI backends (BYO, Ollama, bundled local llama.cpp)
│       ├── ai_history/            # Chat conversation persistence
│       ├── ai_presets/            # User-defined keyword → prompt AI commands
│       ├── context/               # Focused-window / project / git context awareness
│       ├── file_search/           # Fuzzy file index + frecency ranking
│       ├── files/                 # File operations (convert, zip, extract)
│       ├── db/                    # redb tables + migrations
│       ├── config/                # TOML config (default-overlay merge) + syncable subset
│       ├── history/               # Command history
│       ├── notes/ clipboard/ quicklinks/ snippets/ aliases/ reminders/
│       ├── script_commands/       # User scripts from ~/.config/lychi/scripts
│       ├── desktop_apps/          # XDG .desktop discovery
│       ├── events/                # Internal event bus
│       ├── mpris.rs               # MPRIS D-Bus media control (feature-gated)
│       ├── error.rs               # LychiError (thiserror)
│       └── paths.rs               # XDG directory resolution
├── src-tauri/                     # Tauri app — thin bridge layer
│   └── src/
│       ├── lib.rs                 # Tauri Builder setup
│       ├── state.rs               # AppState + handler registration
│       ├── window.rs              # Show/hide/toggle + monitor repositioning
│       ├── platform/              # Platform abstraction (linux.rs — GTK/GDK/layer-shell)
│       ├── hotkey_portal.rs       # XDG GlobalShortcuts portal
│       ├── ipc_server.rs          # Unix domain socket IPC
│       ├── reactors.rs            # Event-driven side effects (e.g. AI hot-swap)
│       └── commands/              # #[tauri::command] IPC wrappers
├── cli/                           # CLI binary (`lychi start|--toggle|…`)
├── src/                           # Svelte 5 frontend (SvelteKit SPA)
├── scripts/                       # Build post-processing
└── package.json                   # Frontend deps + all scripts
```

## All scripts reference

| Script | Command | Description |
|--------|---------|-------------|
| `pnpm dev` | `RUST_LOG=debug tauri dev` | Launch app with hot reload |
| `pnpm dev:local-ai` | `tauri dev --features local-ai` | Hot reload with bundled local AI |
| `pnpm build` | build CLI → `tauri build` → codec fixup | Build production AppImage |
| `pnpm build:debug` | `tauri build --debug` | Unoptimised bundle |
| `pnpm build:cli` | `cargo build --release -p lychi-cli` | Build the CLI binary only |
| `pnpm test` | `cargo test --workspace` | All Rust tests |
| `pnpm test:core` | `cargo test -p lychi-core` | Core library tests |
| `pnpm test:fe` | `vitest run` | Frontend tests |
| `pnpm check` | `svelte-check` | TypeScript/Svelte type check |
| `pnpm lint` / `lint:fix` | `biome check src/` | Lint frontend (optionally auto-fix) |
| `pnpm format` | `biome format --write src/` | Format frontend |
| `pnpm format:rust` | `cargo fmt --all` | Format Rust |
| `pnpm clippy` | `cargo clippy --workspace` | Lint Rust |
| `pnpm clean` | `rm -rf .svelte-kit node_modules/.vite` | Clear frontend caches |

## Contributing

Pull requests are welcome — see [CONTRIBUTING.md](CONTRIBUTING.md) for setup, architecture rules, and performance budgets.

Good places to start:

- **A new command.** The easiest contribution by design: implement `ActionHandler` in one file under `crates/lychi-core/src/action_registry/handlers/`, add one registration line in `src-tauri/src/state.rs`. No frontend changes needed.
- **Desktop-environment fixes.** If you use a desktop where something is off and you can debug it, that's high-value work — see `src-tauri/src/platform/linux.rs`.
- **Packaging.** AUR, Flatpak, and nixpkgs packaging would all help; the AppImage is currently the only distribution channel.

Two things worth knowing before a large PR: business logic belongs in `crates/lychi-core` and must compile without Tauri, and Lychi is local-first — features that require network access or send data off the machine need explicit user opt-in.

## Security

Please report vulnerabilities privately — see [SECURITY.md](SECURITY.md).

## Changelog

See [CHANGELOG.md](CHANGELOG.md).

## License

[GPL-3.0](LICENSE) — forks and contributions must remain open source.

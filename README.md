# Lychi

A local-first, Linux-only desktop command launcher. Think Spotlight/Raycast for Linux — keyboard-driven, privacy-friendly, AI optional (bundled local model, BYO key, or Ollama).

Built with **Tauri v2** (Rust backend) + **Svelte 5** (SvelteKit frontend), distributed as an **AppImage**.

## Desktop Environment Support

Lychi picks the best window strategy for your session automatically:

| Session | Strategy |
|---------|----------|
| wlroots compositors (Hyprland, Sway, …) | wlr-layer-shell overlay |
| KDE Plasma Wayland | toplevel window (KWin layer-shell focus is unreliable) |
| GNOME Wayland | monitor-covering transparent toplevel (Mutter has no layer-shell) |
| X11 (KDE, XFCE, Cinnamon, MATE, …) | fullscreen overlay, or a compact opaque window when compositing is off |

On GNOME the window covers the monitor with a transparent surface and the launcher is centered by CSS — Mutter does not let applications position their own windows. True fullscreen is deliberately **not** requested there, because Mutter paints an opaque backdrop behind fullscreen windows.

**Global hotkey:** Lychi registers `Super+Space` for you where it can, by three
routes in order of preference:

| Session | How |
|---------|-----|
| GNOME, KDE (Wayland) | XDG GlobalShortcuts portal — the compositor asks you to confirm |
| XFCE, GNOME, Cinnamon (X11) | written into the desktop's own keyboard settings |
| Everything else | X11 key-grab, or bind `lychi --toggle` yourself |

An X11 key-grab alone is not enough: it cannot override a combination the window
manager already owns, and it fails *silently* — which is why the desktop's own
settings are written too. If the key you picked is already taken, Lychi says so
in the log and leaves the existing binding alone rather than stealing it; pick
another in Settings, or bind `lychi --toggle` manually.

## Prerequisites

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

## Setup

```bash
# Clone the repo
git clone https://github.com/Shansabry/lychi-core.git
cd lychi-core

# Install frontend dependencies
pnpm install

# Verify Rust toolchain
rustc --version   # should be >= 1.93.0
cargo tauri --version
```

## Development

```bash
# Launch the app with hot reload (frontend + backend)
pnpm dev

# Same, with the bundled local AI compiled in (slower first build)
pnpm dev:local-ai

# Run all Rust tests
pnpm test

# Run core library tests only
pnpm test:core

# Run frontend tests (Vitest)
pnpm test:fe
```

## Code Quality

### Rust

```bash
pnpm format:rust   # cargo fmt --all
pnpm clippy        # cargo clippy --workspace
cargo check        # type check all crates
```

### TypeScript / Svelte

```bash
pnpm lint          # Biome lint
pnpm lint:fix      # Biome lint + auto-fix
pnpm format        # Biome format
pnpm check         # svelte-check
```

## Build

```bash
# Build production AppImage
pnpm build
```

This builds the CLI binary, runs `tauri build` with the `local-ai` feature and `NO_STRIP=1`, then post-processes the AppImage with `scripts/fix-appimage-codecs.sh`.

The output lands in **`target/release/bundle/appimage/`** (a Cargo workspace shares one `target/` at the root).

> **The post-build step is not optional.** Tauri's linuxdeploy bundles WebKitGTK's entire transitive GStreamer stack, whose ELF constructors crash the dynamic loader at `_dl_init` on hosts with a newer glibc than the build box. `fix-appimage-codecs.sh` keeps only the `dlopen`'d GTK plugins (input-method, pixbuf, print), drops the shared libraries, and repacks — which also takes the AppImage from ~114 MB to ~16 MB. Anything it keeps or drops is an explicit policy in that file, deliberately **not** a scan of the build machine.

## Project Structure

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
│       ├── coordinator/           # Cross-brick orchestration
│       ├── providers/             # AI backends (BYO, Ollama, bundled local llama.cpp)
│       ├── ai_history/            # Chat conversation persistence
│       ├── ai_presets/            # User-defined keyword → prompt AI commands
│       ├── context/               # Focused-window / project / git context awareness
│       ├── file_search/           # Fuzzy file index + frecency ranking
│       ├── files/                 # File operations (convert, zip, extract)
│       ├── db/                    # redb tables + migrations
│       ├── config/                # TOML config (default-overlay merge) + syncable subset
│       ├── history/               # Command history
│       ├── notes/                 # Notes & todos
│       ├── clipboard/             # Clipboard history + image store
│       ├── quicklinks/            # Parameterized user commands
│       ├── script_commands/       # User scripts from ~/.config/lychi/scripts
│       ├── snippets/ aliases/ reminders/
│       ├── desktop_apps/          # XDG .desktop discovery
│       ├── events/                # Internal event bus
│       ├── mpris.rs               # MPRIS D-Bus media control (feature-gated)
│       ├── fonts.rs               # Installed font enumeration (fontconfig)
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
├── src/                           # Svelte 5 frontend
│   ├── lib/
│   │   ├── ipc.ts                 # Typed wrappers for Tauri invoke()
│   │   ├── keybindings.ts         # Shortcut table (mirrors config.keybindings)
│   │   └── components/            # Svelte components
│   └── routes/                    # SvelteKit pages (SPA)
├── scripts/                       # Build post-processing
└── package.json                   # Frontend deps + all scripts
```

## Built-in Commands

Around 45 handlers ship built in. The most-used ones:

| Command | Example | Action |
|---------|---------|--------|
| `open` | `open firefox` | Launch a desktop application (XDG .desktop discovery) |
| `web` | `web rust lang` | Search the web (custom engines + bangs supported) |
| `yt` | `yt lofi beats` | Search YouTube |
| `run` | `run ls -la` | Execute a shell command (gated by the shell rules engine) |
| `calc` | `calc 2+2` | Math, unit and currency conversion |
| `file` | `/ report.pdf` | Fuzzy file search — open, reveal, or drill into folders |
| `project` | `project lychi` | Open a project directory in your editor |
| `media` | `media pause all` | Control any MPRIS player (Spotify, browsers, VLC, …) |
| `clip` | `cl: token` | Browse and paste from clipboard history |
| `note` / `todo` | `note pick up milk` | Quick notes and todos |
| `quicklink` | `gh lychi` | Your own parameterized commands (URL, shell, path, or Lychi command) |
| `script` | `deploy` | Run a Script Command from `~/.config/lychi/scripts/` |
| `screenshot` | `screenshot area` | Capture full screen, region, or window |
| `packages` | `packages install ripgrep` | Search/install/remove packages (dnf, apt, pacman, zypper, flatpak) |
| `service` | `service restart nginx` | Control systemd services |
| `win` | `win code` | Switch between open windows |
| `devutil` | `devutil base64 hi` | base64, hash, urlencode, epoch, json, text-case |
| `sysinfo` | `si: mem` | ip, cpu, mem, disk, temp, gpu, battery, net, audio, display, os |
| `snip` / `alias` | `sn: sig` | Snippets and command aliases |
| `timer` / `reminder` | `tm: 5m tea` | Timers and reminders (persist across restarts) |
| `generate` | `generate uuid` | Passwords, UUIDs, tokens, random numbers |
| `color` | `#3b82f6` | Convert hex/RGB/HSL, nearest Tailwind match |
| `emoji` / `unicode` / `qr` | `e: rocket` | Emoji picker, Unicode search, QR codes |
| `define` / `weather` / `ssh` / `bm` | `define ephemeral` | Dictionary, weather, SSH hosts, browser bookmarks |

### Trigger characters

| Input | Routes to |
|-------|-----------|
| `=` | Math / unit / currency expression |
| `>` | Shell command |
| `/` | Fuzzy file search |
| `~/` | Open a path from home |
| `@` | Reference a file inside a command |
| `#hex` | Preview a hex colour |
| `example.com` | Open a URL |

Two-letter colon prefixes route directly to a handler: `bm:`, `cl:`, `sym:`, `sys:`, `si:`, `yt:`, `e:`, `u:`, `w:`, `r:`, `c:`, `f:`, `o:`, `n:`, `m:`, `p:`, `tz:`, `al:`, `sn:`, `tm:`, `rm:`.

With AI enabled, natural-language input is routed to the right command automatically. Settings → Guide lists every command and trigger, generated from the live registry.

## Keyboard Shortcuts

All shortcuts are user-configurable under `[keybindings]` in `config.toml`. Defaults:

| Shortcut | Action |
|----------|--------|
| `Ctrl+K` | Action panel for the selected result |
| `Ctrl+1` | Toggle history panel |
| `Ctrl+2` | Toggle notes panel |
| `Ctrl+3` | Toggle media panel |
| `Ctrl+4` | Toggle settings panel |
| `Enter` | Submit |
| `Escape` | Dismiss window / close panel |
| `Tab` / `Shift+Tab` | Accept completion / step back |
| `Ctrl+Tab` | Switch scope |
| `Up` / `Down` | Navigate results and history |
| `Ctrl+Enter` | Search the web with the current input |
| `Shift+Enter` | Run in a new terminal |
| `Ctrl+O` | Open inline URL |
| `Ctrl+Shift+C` | Copy path |
| `Ctrl+Shift+S` | Screenshot |
| `Ctrl+Shift+A` | Attach file |

## CLI

```bash
lychi start                      # start Lychi (no-op if already running)
lychi --toggle                   # show/hide the launcher
lychi --screenshot [area|window] # capture without opening the launcher
lychi --ai [preset]              # run an AI preset on the selected text
lychi --help
```

Every verb except `start` talks to a running instance over a Unix socket, so they're cheap to bind to desktop shortcuts.

## Config

Config lives at `~/.config/lychi/config.toml`. Every field has a default — a missing or empty file is valid, and unknown keys are merged over the defaults rather than rejected.

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

## Data Files

| Path | Purpose |
|------|---------|
| `~/.config/lychi/config.toml` | App configuration |
| `~/.config/lychi/scripts/` | Script Commands (any executable becomes a command) |
| `~/.local/share/lychi/lychi.redb` | History, notes, todos, clipboard, snippets, aliases, reminders, timers, AI presets & chats |
| `~/.local/share/lychi/models/` | Downloaded local AI weights |
| `~/.local/share/lychi/clipboard-images/` | Clipboard image store |
| `~/.local/share/lychi/logs/` | Rotating JSON logs |

## Architecture

- **`lychi-core`** — All business logic. Zero Tauri knowledge. Testable in isolation. Organised as LEGO bricks: action_registry, rules, intent, executor, coordinator, providers.
- **`src-tauri`** — Thin Tauri shell. Bridges core to frontend via IPC commands (5–10 lines each).
- **`cli`** — Tiny binary that talks to the running app over a Unix domain socket.

Commands are extensible via the `ActionHandler` trait + `ActionRegistry` dispatch. Adding a new command = 1 new handler file + 1 registration line in `state.rs`; no frontend changes are needed.

Every execution passes through the rules engine, which owns the shell/path/URI permission decisions in one place. AI suggests actions but never executes them directly.

## All Scripts Reference

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

## Testing and feedback

Lychi is pre-1.0 and solo-maintained. The single most useful thing you can do is **run it on your setup and say what happened** — especially if your desktop, compositor, or distro isn't a common one.

Lychi aims to behave identically on every Linux desktop, and window management is where they differ most: placement, focus, transparency, and dismiss behaviour are all compositor-dependent. Bugs there are close to impossible to find without reports from the desktop they happen on.

Useful things to report:

- The launcher appears in the wrong place, at the wrong size, or covers the screen
- It won't take keyboard focus, or won't dismiss when it should
- A command didn't do what its name suggests
- You opened it, didn't know what to type, and closed it again — genuinely useful, and hard to see from the inside

[Open an issue](https://github.com/Shansabry/lychi-core/issues) — the bug form asks for your distro, desktop, and session type, which is usually enough to reproduce. Running Lychi from a terminal and pasting the first ~20 lines of output helps too; those lines show which window strategy was picked.

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

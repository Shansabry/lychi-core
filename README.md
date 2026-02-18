# Lychi

A local-first, Linux-first desktop command launcher. Think Spotlight/Raycast for Linux.

Built with **Tauri v2** (Rust backend) + **Svelte 5** (SvelteKit frontend), distributed as an **AppImage**.

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
git clone <repo-url>
cd lychi/core

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

# Run all tests
pnpm test

# Run core library tests only
pnpm test:core
```

## Code Quality

### Rust

```bash
# Format Rust code
pnpm format:rust

# Lint with Clippy
pnpm clippy

# Type check all crates
cargo check
```

### TypeScript / Svelte

```bash
# Lint with Biome
pnpm lint

# Lint and auto-fix
pnpm lint:fix

# Format frontend code
pnpm format

# Svelte type check
pnpm check
```

## Build

```bash
# Build production AppImage
pnpm build
```

The AppImage will be output to `src-tauri/target/release/bundle/appimage/`.

## Project Structure

```
core/
├── Cargo.toml                     # Workspace root (3 crates)
├── crates/lychi-core/             # Core library — all business logic
│   └── src/
│       ├── command/               # CommandHandler trait + implementations
│       ├── history/               # Command history (JSON persistence)
│       ├── config/                # App config (TOML with serde defaults)
│       ├── error.rs               # LychiError (thiserror)
│       └── paths.rs               # XDG directory resolution
├── src-tauri/                     # Tauri app — thin bridge layer
│   └── src/
│       ├── lib.rs                 # Tauri Builder setup
│       ├── state.rs               # AppState + handler registration
│       └── commands/              # #[tauri::command] IPC wrappers
├── cli/                           # CLI binary for `lychi --toggle`
├── src/                           # Svelte 5 frontend
│   ├── lib/
│   │   ├── ipc.ts                 # Typed wrappers for Tauri invoke()
│   │   └── components/            # Svelte components
│   └── routes/                    # SvelteKit pages (SPA)
├── biome.json                     # Biome formatter/linter config
├── rustfmt.toml                   # Rust formatter config
├── clippy.toml                    # Clippy linter config
└── package.json                   # Frontend deps + all scripts
```

## Built-in Commands

| Prefix | Example | Action |
|--------|---------|--------|
| `open` | `open firefox` | Launch a desktop application (XDG .desktop discovery) |
| `web` | `web rust lang` | Search the web (opens default browser) |
| `yt` | `yt lofi beats` | Search YouTube |
| `run` | `run ls -la` | Execute a shell command |

## Config

Config lives at `~/.config/lychi/config.toml`. All fields have defaults — missing or empty file is valid.

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
```

## Architecture

- **`lychi-core`** — All business logic. Zero Tauri knowledge. Testable in isolation.
- **`src-tauri`** — Thin Tauri shell. Bridges core to frontend via IPC commands (5-10 lines each).
- **`cli`** — Tiny binary for `lychi --toggle` via Unix domain socket.

Commands are extensible via the `CommandHandler` trait + `CommandRegistry` dispatch pattern. Adding a new command = 1 new file + 1 registration line, no frontend changes needed.

## All Scripts Reference

| Script | Command | Description |
|--------|---------|-------------|
| `pnpm dev` | `tauri dev` | Launch app with hot reload |
| `pnpm build` | `tauri build --bundles appimage` | Build production AppImage |
| `pnpm test` | `cargo test --workspace` | Run all Rust tests |
| `pnpm test:core` | `cargo test -p lychi-core` | Run core library tests |
| `pnpm check` | `svelte-check` | TypeScript/Svelte type check |
| `pnpm lint` | `biome check src/` | Lint frontend code |
| `pnpm lint:fix` | `biome check --write src/` | Lint + auto-fix frontend |
| `pnpm format` | `biome format --write src/` | Format frontend code |
| `pnpm format:rust` | `cargo fmt --all` | Format Rust code |
| `pnpm clippy` | `cargo clippy --workspace` | Lint Rust code |

## License

UNLICENSED

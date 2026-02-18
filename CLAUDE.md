# Lychi Core — Desktop App

## Tech Stack

- **Backend**: Rust (edition 2024), Tauri v2
- **Frontend**: Svelte 5 (SvelteKit, adapter-static, SPA mode)
- **Runtime**: Tokio (async)
- **Distribution**: AppImage (Linux)

## Project Structure

```
core/
├── Cargo.toml                     # Workspace root (3 crates)
├── crates/lychi-core/             # Core library — ALL business logic
│   └── src/
│       ├── command/               # CommandHandler trait + implementations
│       │   ├── mod.rs             # Trait, CommandInput, CommandResult
│       │   ├── registry.rs        # CommandRegistry (dispatch)
│       │   ├── parser.rs          # Parse "open firefox" → (prefix, args)
│       │   ├── app_launcher.rs    # "open" — XDG .desktop discovery
│       │   ├── web_search.rs      # "web" — browser search
│       │   ├── youtube.rs         # "yt" — YouTube search
│       │   └── shell_exec.rs      # "run" — sh -lc execution
│       ├── history/               # JSON file persistence, dedup
│       ├── config/                # TOML config with serde defaults
│       ├── error.rs               # LychiError (thiserror)
│       └── paths.rs               # XDG directory resolution
├── src-tauri/                     # Tauri app — THIN bridge layer
│   └── src/
│       ├── lib.rs                 # Tauri Builder setup
│       ├── main.rs                # Entry point
│       ├── state.rs               # AppState + handler registration
│       └── commands/              # #[tauri::command] wrappers
├── cli/                           # CLI binary for `lychi --toggle`
├── src/                           # Svelte 5 frontend
│   ├── lib/
│   │   ├── ipc.ts                 # Typed wrappers for Tauri invoke()
│   │   ├── components/            # Svelte components
│   │   └── stores/                # Svelte 5 runes ($state, $effect)
│   └── routes/                    # SvelteKit pages (SPA, SSR disabled)
├── package.json                   # Frontend dependencies
├── svelte.config.js               # adapter-static, SPA fallback
└── vite.config.ts
```

## Architecture Rules

### Crate Boundaries

| Crate | Knows About | Never Touches |
|-------|-------------|---------------|
| `lychi-core` | Business logic, Tokio, Serde | Tauri, frontend, window management |
| `src-tauri` (lychi-app) | Tauri, lychi-core | Business logic details |
| `cli` (lychi-cli) | Unix sockets | Everything else |

- `lychi-core` must be testable WITHOUT Tauri. No Tauri imports allowed.
- `src-tauri` commands are thin wrappers (5-10 lines max). Extract state, call core, return.
- All business logic lives in `lychi-core`. No exceptions.

### Adding a New Command

1. Create `crates/lychi-core/src/command/my_command.rs`
2. Implement the `CommandHandler` trait
3. Add `pub mod my_command;` to `command/mod.rs`
4. Register in `src-tauri/src/state.rs`: `registry.register(Box::new(MyCommand::new()))`
5. No frontend changes needed — dispatch is automatic via the registry

### Adding an Integration (e.g., Slack)

1. Add feature flag in `crates/lychi-core/Cargo.toml`: `slack = ["dep:reqwest"]`
2. Create module behind `#[cfg(feature = "slack")]`
3. Conditionally register in `src-tauri/src/state.rs`
4. Forward feature in `src-tauri/Cargo.toml`: `slack = ["lychi-core/slack"]`

### State Management

- **Rust**: Single `AppState` struct with `Arc<RwLock<>>` fields, registered via Tauri `.manage()`
- **Frontend**: Svelte 5 runes (`$state`, `$effect`). No external state library.
- **IPC**: All Tauri calls go through `src/lib/ipc.ts` — single source of truth for command contracts

## Code Style — Rust

### Formatting

- Use `rustfmt` defaults (install `rustfmt` if missing: `rustup component add rustfmt`)
- Tabs: spaces, width 4
- Max line width: 100 (rustfmt default)
- Run `cargo fmt` before committing

### Naming

- Crates: `kebab-case` (`lychi-core`)
- Modules: `snake_case` (`app_launcher.rs`)
- Types/Traits: `PascalCase` (`CommandHandler`, `AppState`)
- Functions/Methods: `snake_case` (`execute_command`)
- Constants: `SCREAMING_SNAKE_CASE` (`DEFAULT_SEARCH_URL`)

### Error Handling

- Per-module error types using `thiserror` in `lychi-core`
- `LychiError` enum wraps all module errors
- `LychiError` implements `Serialize` for Tauri IPC
- Use `anyhow` only in top-level app code (main.rs), never in library code
- Always use `Result<T, LychiError>`, never `unwrap()` in production code
- `unwrap()` / `expect()` are allowed only in tests and initialization code that should panic

### Async

- Use `async-trait` for async trait methods
- Use `tokio::sync::RwLock` (not `std::sync::Mutex`) in async contexts
- Tauri commands that call async code must be `async fn`

### Dependencies

- Keep dependencies minimal — every new dep increases compile time
- Optional deps behind feature flags: `dep:crate_name` syntax
- All shared dependency versions in workspace `[workspace.dependencies]`
- Check `cargo outdated` periodically (install: `cargo install cargo-outdated`)

### Testing

- Tests live next to the code they test (`#[cfg(test)] mod tests`)
- Run: `cargo test -p lychi-core`
- Core logic must be testable without Tauri
- Use `#[tokio::test]` for async tests
- Temp files in tests: use unique paths with atomic counter to avoid parallel test conflicts

## Code Style — TypeScript / Svelte

### Formatting & Linting

- Use **Biome** (`biome.json`) for formatting and linting — NOT Prettier/ESLint
- Tabs: tabs (indentWidth: 2)
- Max line width: 100
- Run `pnpm lint:fix` to auto-fix lint issues
- Run `pnpm format` to format frontend code

### Svelte 5 Patterns

- Use runes: `$state`, `$derived`, `$effect`, `$props`, `$bindable`
- Do NOT use legacy Svelte 4 patterns (`let:`, `$:`, `export let`)
- Props: `let { prop1, prop2 }: { prop1: Type; prop2: Type } = $props()`
- Events: pass callback props (`onsubmit`, `onclick`), not `createEventDispatcher`

### Component Structure

```svelte
<script lang="ts">
  // 1. Imports
  // 2. Props
  // 3. State
  // 4. Effects
  // 5. Functions
</script>

<!-- Template -->

<style>
  /* Scoped styles */
</style>
```

### IPC Layer

- All Tauri `invoke()` calls go through `src/lib/ipc.ts`
- Every IPC function has explicit TypeScript types matching the Rust return types
- Never call `invoke()` directly from components

### Styling

- CSS variables defined in `src/app.css`
- Scoped `<style>` blocks in components
- No CSS frameworks — minimal black/white theme
- Font stack: system sans-serif for UI, monospace for command input/output

## Commands Reference

```bash
# Development
cd core
cargo tauri dev              # Launch app with hot reload
cargo test -p lychi-core     # Run core tests
cargo check                  # Type check all crates
cargo fmt                    # Format Rust code
cargo clippy --workspace     # Lint Rust code
pnpm dev                     # Frontend dev server only
pnpm build                   # Build frontend to build/
pnpm check                   # Svelte type check
pnpm lint                    # Biome lint check
pnpm lint:fix                # Biome lint auto-fix
pnpm format                  # Biome format

# Build
cargo tauri build --bundles appimage   # Production AppImage

# Maintenance
cargo outdated -R            # Check for outdated deps
cargo update                 # Update Cargo.lock
```

## Config & Data Paths

- Config: `~/.config/lychi/config.toml` (TOML, serde defaults for all fields)
- History: `~/.local/share/lychi/history.json`
- Determined via `directories` crate (XDG-compliant)

## Key Design Decisions

1. **3 crates, not more** — enough separation, minimal overhead for solo dev
2. **Trait objects for commands** — `Box<dyn CommandHandler>` in a HashMap registry
3. **Feature flags for integrations** — compile-time inclusion without crate explosion
4. **Flat files for MVP** — JSON history, TOML config. SQLite in Phase 2+
5. **SvelteKit SPA** — SSR disabled, adapter-static, single page app
6. **No CSS framework** — minimal custom CSS, premium feel

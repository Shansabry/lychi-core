# Lychi Core — Desktop App

## Tech Stack

- **Backend**: Rust (edition 2024), Tauri v2
- **Frontend**: Svelte 5 (SvelteKit, adapter-static, SPA mode)
- **Runtime**: Tokio (async)
- **Distribution**: AppImage (Linux)

## Project Structure

```
core/
├── Cargo.toml                     # Workspace root
├── crates/lychi-core/             # Core library — ALL business logic
│   └── src/
│       ├── action_registry/       # BRICK: Action Registry
│       │   ├── mod.rs             # ActionHandler trait, ActionResult, RiskLevel, CompletionItem
│       │   ├── registry.rs        # ActionRegistry (register, lookup, list)
│       │   └── handlers/          # All handler implementations
│       │       ├── app_launcher.rs
│       │       ├── calc.rs
│       │       ├── file_open.rs
│       │       ├── icons.rs
│       │       ├── project_open.rs
│       │       ├── shell_exec.rs
│       │       ├── spotify.rs
│       │       ├── system.rs
│       │       ├── url_open.rs
│       │       ├── web_search.rs
│       │       └── youtube.rs
│       ├── rules/                 # BRICK: Rules Engine
│       │   ├── mod.rs             # RulesEngine, ValidationDecision, ValidationRequest
│       │   └── shell.rs           # ShellRules (denylist, dangerous/moderate patterns)
│       ├── intent/                # BRICK: Intent Resolver
│       │   ├── mod.rs             # IntentResolver (pattern + AI routing)
│       │   ├── patterns.rs        # Deterministic pattern matching
│       │   ├── ai_router.rs       # AiRouter wrapper
│       │   └── prompt.rs          # System prompt + response parsing
│       ├── executor/              # BRICK: Execution Manager
│       │   └── mod.rs             # Executor (resolve → validate → execute)
│       ├── providers/             # BRICK: AI Providers
│       │   ├── mod.rs             # AiProvider trait (chat/health_check/name)
│       │   └── byo.rs             # BYOClient (OpenAI/Anthropic/Groq)
│       ├── coordinator/           # The streaming tool-calling agent loop
│       ├── suggestions/           # rank(): the ONE order/consent/default decider
│       ├── intent/                # (see BRICK above) submit routing + classify
│       ├── context/               # Window/IDE/terminal/git context gathering
│       ├── file_search/           # nucleo-backed file index + ranking
│       ├── desktop_apps/          # .desktop parsing + app match index
│       ├── clipboard/             # Clipboard history (privacy-aware)
│       ├── db/                    # redb store + frecency (postcard-encoded rows)
│       ├── history/               # Command history (redb-backed, dedup)
│       ├── config/                # TOML config with serde defaults
│       ├── ai_history/ ai_presets/ notes/ reminders/ snippets/ pins/ aliases/
│       │                          # Feature stores (each a small brick)
│       ├── backup/ setup/ install/ hotkey/ events/                   # Supporting bricks
│       ├── error.rs               # LychiError (thiserror)
│       ├── mpris.rs               # MPRIS D-Bus media control (feature-gated)
│       ├── process_tracker.rs     # PID+start-time process identity
│       ├── fs_atomic.rs           # Crash-safe atomic file writes
│       └── paths.rs               # XDG directory resolution
│   (28 module dirs + ~14 top-level files; the above is the load-bearing subset —
│    see docs/architecture.md for the full brick table)
├── src-tauri/                     # Tauri app — THIN bridge layer
│   └── src/
│       ├── lib.rs                 # Tauri Builder setup
│       ├── main.rs                # Entry point
│       ├── state.rs               # AppState + Executor setup
│       ├── ipc_server.rs          # Unix socket IPC listener
│       ├── platform/              # Platform abstraction layer
│       │   ├── mod.rs             # cfg-gated re-exports
│       │   └── linux.rs           # Linux: GTK/GDK, layer-shell, XDG
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

See `docs/architecture.md` for the full LEGO-brick architecture diagram and dependency rules.

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

### Adding a New Action Handler

1. Create `crates/lychi-core/src/action_registry/handlers/my_action.rs`
2. Implement the `ActionHandler` trait (with `id()`, `description()`, `execute()`, optionally `default_risk()` and `completions()`)
3. Add `pub mod my_action;` to `action_registry/handlers/mod.rs`
4. Register in `src-tauri/src/state.rs`: `registry.register(Box::new(MyAction::new()))`
5. No frontend changes needed — dispatch is automatic via the registry
6. Rules Engine auto-validates based on `default_risk()` (Low = auto-execute, Medium/High = confirm)

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
- Types/Traits: `PascalCase` (`ActionHandler`, `AppState`)
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
- State DB: `~/.local/share/lychi/lychi.redb` — a single redb database holding
  history, frecency, clipboard, notes/todos, reminders, snippets, aliases, and
  config mirror. Rows are postcard-encoded and versioned (`[ver][body]` envelope);
  see `db/` and each feature's `store.rs`. (`0600`; a corrupt DB recovers to a
  timestamped `.bak` rather than nuking state.)
- Determined via `directories` crate (XDG-compliant)

## Key Design Decisions

1. **LEGO-brick architecture** — each module (action_registry, rules, intent, executor, providers) is a self-contained brick with clean interfaces, no cross-module shortcuts, and plug-and-play replaceability
2. **Trait objects for actions** — `Box<dyn ActionHandler>` in a HashMap registry
3. **Rules Engine gates every execution** — denylist, risk levels, confirmation flow. AI suggests, never executes directly
4. **Executor is the single orchestrator** — resolve → validate → execute pipeline. Tauri bridge calls only the Executor
5. **Feature flags for integrations** — compile-time inclusion without crate explosion (e.g. `mpris` feature for D-Bus media control)
6. **Platform abstraction** — all platform-specific code (GTK, GDK, layer-shell, XDG sockets) lives in `src-tauri/src/platform/linux.rs`. Adding macOS/Windows means creating one new file per platform — zero changes to callers
7. **redb for state, TOML for config** — one embedded `lychi.redb` (postcard rows,
   versioned envelopes) holds all mutable state; config stays human-editable TOML.
   (History began as a JSON file in the MVP; it moved into redb with the rest.)
8. **SvelteKit SPA** — SSR disabled, adapter-static, single page app
9. **No CSS framework** — minimal custom CSS, premium feel

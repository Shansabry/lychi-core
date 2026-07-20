# Contributing to Lychi

Thanks for your interest! Lychi is a solo-maintained project that welcomes issues and PRs.

## Dev setup

```bash
# Linux prerequisites (Fedora example; see Tauri v2 docs for other distros)
sudo dnf install webkit2gtk4.1-devel gtk3-devel gtk-layer-shell-devel

pnpm install
cargo tauri dev        # runs the app with hot reload (Vite on port 42352)
```

## Before you open a PR

```bash
cargo fmt                  # format Rust
cargo clippy --workspace   # lint Rust
cargo test -p lychi-core   # core tests must pass
pnpm check                 # Svelte/TS type check
pnpm lint:fix              # Biome (not Prettier/ESLint)
```

## Architecture rules (enforced in review)

Read `CLAUDE.md` in the repo root for the full picture. The essentials:

- **`crates/lychi-core`** holds ALL business logic. It must compile and test without Tauri — no Tauri imports, ever.
- **`src-tauri`** is a thin bridge: `#[tauri::command]` wrappers of 5–10 lines that extract state, call core, return.
- **Platform-specific code** (GTK, layer-shell, X11) lives only in `src-tauri/src/platform/`.
- **Frontend**: Svelte 5 runes only (no Svelte 4 patterns), all IPC through `src/lib/ipc.ts`, no CSS frameworks, no state libraries.
- **New action handlers**: implement the `ActionHandler` trait in `crates/lychi-core/src/action_registry/handlers/`, register it in `src-tauri/src/state.rs` — no frontend changes needed.

## Performance budgets (non-negotiable)

- Launcher opens in < 100ms; no loading screens
- < 150MB memory; idle-memory regressions > 10MB need justification
- Never create/destroy DOM in hot paths — keep nodes alive and toggle `visibility` (WebKitGTK first-paint costs are severe; see existing patterns in `CompletionsList.svelte`)
- AI calls are async/background only; everything must work offline

## Privacy rules

Local-first is the product. Nothing leaves the machine without explicit user opt-in. No telemetry. Network features (weather geolocation, public-IP lookup) are consent-gated — follow the existing `grant_privacy_consent` pattern.

## Commit style

Conventional-ish prefixes as in `git log`: `feat:`, `fix:`, `chore:`, `perf:`, `docs:`.

## License

By contributing, you agree your contributions are licensed under GPL-3.0 like the rest of the project.

# Changelog

All notable changes to Lychi are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
Pre-1.0, minor versions may contain breaking changes.

## [Unreleased]

### Added

- **Launcher core** — fuzzy app launching via XDG `.desktop` discovery, fuzzy
  file search with frecency ranking, shell execution, web and YouTube search
  with user-definable search engines ("bangs"), math/unit/currency evaluation,
  and project opening.
- **~45 built-in commands**, including clipboard history, snippets, aliases,
  notes and todos, timers and reminders, screenshots, systemd service control,
  package management (dnf/apt/pacman/zypper/flatpak), window switching, SSH
  hosts, browser bookmarks, developer utilities (base64/hash/urlencode/epoch/
  json/text-case), QR codes, emoji and Unicode search, colour conversion,
  dictionary, weather, and system info.
- **Quicklinks** — parameterized user-defined commands that expand to a URL,
  shell command, path, or another Lychi command, with escaping applied per
  destination.
- **Script Commands** — any executable in `~/.config/lychi/scripts/` becomes a
  named command, hot-reloaded on change.
- **AI (optional, off by default)** — four modes: disabled, BYO key, Ollama, or
  a bundled local model (llama.cpp, CPU-only). BYO supports Anthropic, OpenAI,
  Groq, Grok, Gemini, OpenRouter, or any custom endpoint; the model is always
  user-typed rather than picked from a baked-in list. Includes a streaming
  tool-calling agent, user-defined AI Commands, chat history, file attachments
  with document and vision support, and running AI over text selected in any
  application.
- **Context awareness** — commands resolve against the focused terminal or IDE
  (working directory, git repository, project), including multi-repository
  workspaces.
- **Ctrl+K action panel** — per-result actions, with fully configurable
  keybindings.
- **Theming** — WCAG-safe accent generation and a font picker that previews
  each installed typeface in itself.
- **Desktop integration** — wlr-layer-shell on wlroots compositors, toplevel
  windows on KDE and GNOME, X11 fallback with a compact mode for
  non-composited sessions; XDG GlobalShortcuts portal for the global hotkey;
  MPRIS media control; tray icon and autostart.
- **CLI** — `lychi start`, `--toggle`, `--screenshot [area|window]`,
  `--ai [preset]`, all over a Unix socket so they're cheap to bind to desktop
  shortcuts.

### Security

- **Central permission deciders** — every execution passes through one decider
  per surface (`rules/shell.rs`, `path.rs`, `uri.rs`), closing two paths where
  script commands and AI-generated plans could reach a shell without the gate.
  API keys are stored in the system keyring, never on disk in plain text.
  BYO endpoints are required to be HTTPS unless they're loopback.

### Fixed

- **GNOME Wayland: blank window on any input.** WebKitGTK initialises a
  GStreamer pipeline in every WebProcess regardless of page content; on hosts
  without `gst-plugins-base` the process died and left the UI blank while the
  app kept running. Lychi has no media elements, so the media stack is now
  switched off entirely rather than the codecs bundled.
- **GNOME Wayland: launcher rendered as an opaque full-screen panel.** Mutter
  paints a black backdrop behind fullscreen windows, which defeated the
  transparent monitor-covering surface the launcher is centred on. Fullscreen
  is no longer requested on Mutter-based desktops.
- **AppImage bundling** now follows an explicit keep-list rather than probing
  the build machine, which had made the artifact depend on which packages the
  builder happened to have installed.
- **CLI verbs** are handled by the AppImage itself, so `lychi --help` prints
  usage instead of launching a second window.
- Token usage is reported for OpenAI-compatible providers.

### Known limitations

- First-run guidance is limited to contextual hints (such as the Wayland hotkey
  banner); there is no guided onboarding.
- The window appears in the taskbar on KDE Wayland
  ([tauri#9829](https://github.com/tauri-apps/tauri/issues/9829)).
- AppImage is currently the only distribution channel.

[Unreleased]: https://github.com/Shansabry/lychi-core/commits/main

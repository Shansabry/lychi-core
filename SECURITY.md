# Security Policy

## Reporting a vulnerability

Please report security issues privately via
[GitHub Security Advisories](https://github.com/Shansabry/lychi-core/security/advisories/new),
not as a public issue.

Lychi is solo-maintained, so response times are best-effort — but security
reports go to the front of the queue. Please give me a reasonable window to
ship a fix before disclosing publicly.

## Supported versions

Only the latest release is supported. Lychi is pre-1.0 and moves quickly.

## What's in scope

Lychi runs shell commands, opens files and URLs, and stores API keys, so the
areas most worth scrutiny are:

- **Command execution** — anything that reaches a shell without passing through
  the rules engine (`crates/lychi-core/src/rules/`). Every execution path is
  supposed to go through one of the deciders in `rules/shell.rs`, `path.rs`,
  or `uri.rs`; a route that bypasses them is a bug even if it looks harmless.
- **Injection through untrusted input** — quicklink templates, script commands,
  AI-generated plans, `@file` references. Values are escaped per destination;
  a case where a value escapes into the template is in scope.
- **Path traversal** — file operations that escape their intended directory.
- **Secret handling** — API keys live in the system keyring. Any path that
  writes one to disk, logs it, or sends it somewhere unintended is in scope.
- **Privacy** — Lychi is local-first. Anything leaving the machine without
  explicit user opt-in is a security issue, not just a bug.

## What's out of scope

- **AI-suggested commands being wrong or dangerous.** AI proposes; the user
  confirms; the rules engine gates. That's the design. A *bypass* of that
  confirmation is in scope — a bad suggestion that the user then approves is not.
- **A user deliberately running a destructive command.** Lychi is a launcher;
  running things is the point. It warns on destructive patterns and refuses a
  denylist, but it isn't a sandbox.
- **Compromised local environment.** If an attacker can already write to
  `~/.config/lychi/scripts/` or your `PATH`, they don't need Lychi.

//! Enforces the one rule that keeps `config` and `executor` from deadlocking:
//! **never hold both guards at once.**
//!
//! This is a source scan rather than a runtime test, and deliberately so. The
//! failure is a deadlock — it needs a specific interleaving of two tasks plus a
//! pending writer to manifest, so a runtime test would hang on a bad day and
//! pass on every other one. What can be checked reliably is the shape of the
//! code that makes the interleaving possible.
//!
//! ## The bug this replaces
//!
//! Both nesting orders existed:
//!
//! - `config` → `executor` in `get_ai_status`, `completions`
//! - `executor` → `config` in `execute_command`, `confirm_execution`,
//!   `execute_agent_plan`
//!
//! That is a cycle. tokio's `RwLock` is **write-preferring**, so it is reachable
//! with readers alone: a pending writer blocks subsequent readers, and
//! `reactors.rs` supplies exactly that writer via `blocking_write`.
//!
//! It survived only by luck. The reactors read config through a *temporary*
//! (`self.config.blocking_read().commands.clone()`), which drops at the end of
//! the statement. Binding it to a variable — a completely ordinary refactor —
//! would have closed the cycle. A convention that depends on nobody introducing
//! a `let` is not a safeguard, which is why this test exists.
//!
//! ## What counts as a violation
//!
//! A `let`-bound guard on one lock, with a lock of the *other* taken while it is
//! still in scope. Temporaries (`state.config.read().await.privacy.clone()`) are
//! fine — they drop at the semicolon — and so is anything after an explicit
//! `drop`, or in a later block.

use std::path::{Path, PathBuf};

/// A `let`-bound guard we must not still be holding when the other lock is taken.
struct Guard {
    /// "config" or "executor"
    lock: &'static str,
    line: usize,
    /// Brace depth at the binding, so we know when it goes out of scope.
    depth: i32,
    /// Set by an explicit `drop(name)`.
    dropped: bool,
    name: String,
}

fn source_files() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut out = Vec::new();
    collect(&root, &mut out);
    out.sort();
    out
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// Which of the two locks, if either, this line acquires.
fn lock_acquired(line: &str) -> Option<&'static str> {
    const METHODS: [&str; 4] = [
        ".read()",
        ".write()",
        ".blocking_read()",
        ".blocking_write()",
    ];
    // `.config.read()`, `.executor.blocking_write()`, … — the field name
    // immediately followed by a lock method, so an unrelated `.read()` on some
    // other field is not mistaken for one of these two.
    let takes = |field: &str| {
        METHODS
            .iter()
            .any(|m| line.contains(&format!("{field}{m}")))
    };
    if takes("config") {
        Some("config")
    } else if takes("executor") {
        Some("executor")
    } else {
        None
    }
}

/// Is the acquired guard bound to a name (so it outlives the statement)?
///
/// `let x = ...read().await;` holds. `let x = ...read().await.field.clone();`
/// does not — the guard is a temporary dropped at the semicolon.
fn bound_guard_name(line: &str) -> Option<String> {
    let t = line.trim();
    let rest = t.strip_prefix("let ")?;
    let name = rest
        .trim_start_matches("mut ")
        .split(['=', ':'])
        .next()?
        .trim()
        .to_string();
    if name.is_empty() || name.starts_with('(') {
        return None;
    }
    // A method call after the await means the guard was consumed into a value.
    let after_await = t.split(".await").nth(1)?;
    let is_temporary = after_await.trim_start().starts_with('.');
    if is_temporary { None } else { Some(name) }
}

#[test]
fn config_and_executor_guards_never_overlap() {
    let mut violations: Vec<String> = Vec::new();

    for file in source_files() {
        let text = std::fs::read_to_string(&file).unwrap();
        let rel = file
            .strip_prefix(env!("CARGO_MANIFEST_DIR"))
            .unwrap_or(&file)
            .display()
            .to_string();

        let mut held: Vec<Guard> = Vec::new();
        let mut depth: i32 = 0;

        for (i, raw) in text.lines().enumerate() {
            let line = raw.split("//").next().unwrap_or(raw);
            let lineno = i + 1;

            // Anything whose scope has closed is no longer held.
            held.retain(|g| g.depth <= depth && !g.dropped);

            if let Some(acquired) = lock_acquired(line) {
                // Is a guard on the OTHER lock still alive here?
                if let Some(other) = held
                    .iter()
                    .find(|g| g.lock != acquired && !g.dropped && g.depth <= depth)
                {
                    violations.push(format!(
                        "{rel}:{lineno}: takes `{acquired}` while `{}` (line {}) is still held \
                         — this is the deadlock cycle. Snapshot the first lock's value and let \
                         the guard drop (see AppState::config_snapshot) before taking the second.",
                        other.lock, other.line
                    ));
                }

                if let Some(name) = bound_guard_name(line) {
                    held.push(Guard {
                        lock: acquired,
                        line: lineno,
                        depth,
                        dropped: false,
                        name,
                    });
                }
            }

            // Explicit release.
            if let Some(arg) = line.split("drop(").nth(1).and_then(|r| r.split(')').next()) {
                let arg = arg.trim();
                for g in held.iter_mut() {
                    if g.name == arg {
                        g.dropped = true;
                    }
                }
            }

            depth += line.matches('{').count() as i32;
            depth -= line.matches('}').count() as i32;
            held.retain(|g| g.depth <= depth);
        }
    }

    assert!(
        violations.is_empty(),
        "config/executor lock guards overlap in {} place(s):\n\n{}\n",
        violations.len(),
        violations.join("\n")
    );
}

#[test]
fn the_scanner_detects_an_overlap() {
    // A checker that cannot fail is worthless, and this one only ever asserts
    // an empty list — so prove it recognises the exact shape it is guarding
    // against, using the code that was really there before this was fixed.
    let before_the_fix = r#"
pub async fn execute_command(state: State<'_, AppState>) {
    let executor = state.executor.read().await;
    let privacy = state.config.read().await.privacy.clone();
    executor.run(&privacy).await;
}
"#;
    let mut held: Vec<&str> = Vec::new();
    let mut found = false;
    for raw in before_the_fix.lines() {
        let line = raw.split("//").next().unwrap_or(raw);
        if let Some(acquired) = lock_acquired(line) {
            if held.iter().any(|h| *h != acquired) {
                found = true;
            }
            if bound_guard_name(line).is_some() {
                held.push(acquired);
            }
        }
    }
    assert!(
        found,
        "the scanner failed to flag the original executor→config nesting"
    );
}

#[test]
fn a_temporary_is_not_treated_as_a_held_guard() {
    // The whole reason the old code survived: a guard consumed into a clone
    // drops at the semicolon and is safe. Flagging these would make the test
    // unusable and someone would delete it.
    assert!(
        bound_guard_name("    let privacy = state.config.read().await.privacy.clone();").is_none()
    );
    assert!(bound_guard_name("    let ai = state.config.read().await.ai.clone();").is_none());
    // ...whereas a bare guard IS held.
    assert_eq!(
        bound_guard_name("    let executor = state.executor.read().await;").as_deref(),
        Some("executor")
    );
    assert_eq!(
        bound_guard_name("    let mut config = state.config.write().await;").as_deref(),
        Some("config")
    );
}

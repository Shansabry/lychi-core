//! Contextual suggestion engine ("the suggestion brain").
//!
//! Industry-pattern pipeline, fully local and deterministic:
//!
//! ```text
//! providers (rule-based priors)          — git, project, docker, clipboard…
//!   → learned boost (context-keyed       — Alfred-style latching backed by
//!     acceptance frecency)                 the Firefox frecency formula
//!   → dedupe → confidence gate → cap 8   — Raycast-style shortlist discipline
//! ```
//!
//! Learning loop: the executor records which suggested commands the user
//! actually runs (`sug:{context_key}:{command}` in the frecency store).
//! Next time in the same context (project root, else focused app), accepted
//! suggestions outrank the static rulebook. `typed_matches()` blends the
//! same candidates into normal completions while typing (omnibox-style).
//!
//! No AI, no network, sub-millisecond — ranking must stay explainable and
//! inside the C13 performance budget.

mod providers;
mod scoring;

use std::sync::Arc;

use redb::Database;

use crate::action_registry::CompletionItem;
use crate::db::frecency;

use super::EnvironmentContext;
use providers::{Candidate, ProviderTier, SuggestCtx, providers};

/// Cap on rows shown on the empty prompt.
///
/// Every comparable launcher sits well below what a "show everything" zero
/// state produces: PowerToys Run 4, Superhuman and Flow Launcher 5, Chrome's
/// omnibox 8, Alfred and Ulauncher 9 (hard ceilings). Six keeps the whole list
/// scannable in one glance, which is the entire point of the zero state.
const MAX_COLD_RECENTS: usize = 6;

/// How many recently-used app rows the zero state may show.
///
/// Below the 6-row total so clipboard actions and workspace memory keep a
/// slot — apps must not starve the other two sources. Every launcher surveyed
/// (Raycast, Alfred, Spotlight, PowerToys Run, Ulauncher) sits at or under 9,
/// most under 6.
const MAX_RECENT_APPS: usize = 4;

// ── Tuning ──────────────────────────────────────────────────────────────

/// Suggestions scoring below this after the learned boost are dropped —
/// silence beats junk (a panel of weak guesses trains the user to ignore it).
const CONFIDENCE_GATE: u16 = 55;
/// Hard cap on the shortlist (Raycast-style: few and strong).
const MAX_SUGGESTIONS: usize = 8;
/// No single provider may flood the shortlist.
const MAX_PER_PROVIDER: usize = 4;
/// Maximum score points a fully-learned habit adds on top of the prior.
const LEARNED_BOOST_MAX: f64 = 40.0;
/// Cap on context matches blended into typed completions.
///
/// Deliberately small: context actions share the list with real handler
/// results, and burying an app launch under speculative suggestions is the
/// failure this cap prevents.
const MAX_TYPED_MATCHES: usize = 2;

// ── Suggestion Reason ───────────────────────────────────────────────────

/// Why a particular suggestion was generated. Structured so the engine can
/// produce user-facing strings, debug strings, and scoring features from
/// the same data.
#[derive(Debug, Clone)]
pub enum SuggestionReason {
    /// Git working tree has uncommitted changes.
    GitDirty,
    /// Git working tree is clean — push/pull are the natural next actions.
    GitClean,
    /// On a non-default branch (not main/master).
    GitFeatureBranch { branch: String },
    /// A project script was discovered (npm run dev, cargo build, etc.).
    ProjectScript { runner: String },
    /// Install command for the detected package manager.
    ProjectInstall { pm: String },
    /// Project has a docker-compose.yml / compose.yml.
    DockerCompose,
    /// Docker daemon has running containers.
    DockerRunning { count: usize },
    /// User is deep in a project subdirectory.
    DirectoryDepth { project_name: String },
    /// Clipboard contains actionable content.
    ClipboardContent { content_type: String },
    /// Command previously run in this workspace.
    WorkspaceMemory { project: String },
    /// Pinned workspace is active or detection failed (pin hint).
    PinnedWorkspace,
}

impl SuggestionReason {
    /// Short user-facing string for display in the UI.
    pub fn user_reason(&self) -> String {
        match self {
            Self::GitDirty => "Uncommitted changes".into(),
            Self::GitClean => "Up to date".into(),
            Self::GitFeatureBranch { branch } => format!("On {branch}"),
            Self::ProjectScript { runner } => format!("{runner} script"),
            Self::ProjectInstall { pm } => format!("{pm} project"),
            Self::DockerCompose => "Compose project".into(),
            Self::DockerRunning { count } => {
                format!("{count} container{}", if *count == 1 { "" } else { "s" })
            }
            Self::DirectoryDepth { project_name } => format!("In {project_name}/"),
            Self::ClipboardContent { content_type } => format!("Clipboard: {content_type}"),
            Self::WorkspaceMemory { project } => format!("Recent in {project}"),
            Self::PinnedWorkspace => "Pinned workspace".into(),
        }
    }

    /// Verbose string for `ctx` debug output.
    pub fn debug_reason(&self) -> String {
        match self {
            Self::GitDirty => "git.dirty=true".into(),
            Self::GitClean => "git.dirty=false".into(),
            Self::GitFeatureBranch { branch } => format!("branch={branch} (not main/master)"),
            Self::ProjectScript { runner } => format!("script.runner={runner}"),
            Self::ProjectInstall { pm } => format!("project.pm={pm}"),
            Self::DockerCompose => "project.has_compose=true".into(),
            Self::DockerRunning { count } => format!("docker.containers={count}"),
            Self::DirectoryDepth { project_name } => {
                format!("cwd depth>=1 in {project_name}")
            }
            Self::ClipboardContent { content_type } => {
                format!("clipboard.type={content_type}")
            }
            Self::WorkspaceMemory { project } => format!("workspace={project}"),
            Self::PinnedWorkspace => "workspace.pinned=true".into(),
        }
    }
}

// ── Context key ─────────────────────────────────────────────────────────

/// The learning bucket for the current environment: acceptance is keyed by
/// project root when in a project, else by the focused app, else global.
pub fn context_key(env: &EnvironmentContext) -> String {
    if let Some(ref project) = env.project {
        return format!("proj:{}", project.root.trim_end_matches('/'));
    }
    if let Some(ref win) = env.active_window {
        return format!("app:{}", win.wm_class);
    }
    "global".to_string()
}

/// The workspace root used to key workspace-memory frecency: the project root
/// if in a project, else the cwd. Mirrors `MemoryProvider`'s derivation so the
/// affinity lookup in `rank()` matches the keys memory candidates were stored
/// under. `None` when neither is known.
fn workspace_root(env: &EnvironmentContext) -> Option<String> {
    env.project
        .as_ref()
        .map(|p| p.root.clone())
        .or_else(|| env.cwd.clone())
}

// ── Public API ──────────────────────────────────────────────────────────

/// The zero-state (empty prompt) shortlist. Shows ONLY what the user has
/// actually used or explicitly did — frecency recents, workspace memory, and
/// clipboard actions — never speculative context-derived commands. This is the
/// industry-standard empty-state (Raycast/Alfred/Chrome ZPS): recents cold,
/// context after intent. If nothing qualifies, returns a single hint row.
///
/// Pass `db` to enable recents/learned ranking and workspace memory (None in
/// tests → cold-eligible providers with no db yield nothing).
pub fn suggest(env: &EnvironmentContext, db: Option<&Arc<Database>>) -> Vec<CompletionItem> {
    // Cold-eligible providers: workspace memory (usage-driven) + clipboard
    // (explicit recent action). Ranked with the learned blend.
    let mut items: Vec<CompletionItem> =
        rank(collect(env, db, ProviderTier::ColdEligible), env, db)
            .into_iter()
            .map(|c| c.into_completion_item())
            .collect();

    // Recently-used APPS carry the zero-state. Lychi is a launcher first, and
    // the empty prompt used to show recent COMMAND STRINGS as text — a past
    // `open spotify` rendered as those literal characters with a clock glyph,
    // never as Spotify with the Spotify icon. The app the user wants was one
    // resolution away and nothing performed it.
    //
    // Deduped on `run`, not `label`: launching Spotify writes BOTH the app key
    // (`spotify`, from `app_launcher`) and `history:open spotify` (from the
    // execute command), so a label-keyed set would let "Spotify" and the
    // literal "open spotify" both through — which is the bug, not the fix.
    let mut seen: std::collections::HashSet<String> = items
        .iter()
        .map(|i| i.run.as_deref().unwrap_or(&i.label).to_lowercase())
        .collect();
    for item in recent_apps(db) {
        if items.len() >= MAX_COLD_RECENTS {
            break;
        }
        if seen.insert(item.run.as_deref().unwrap_or(&item.label).to_lowercase()) {
            items.push(item);
        }
    }
    items.truncate(MAX_COLD_RECENTS);

    // Nothing to show (brand-new user / empty context) → one honest hint,
    // never a wall of speculative priors.
    if items.is_empty() {
        return vec![hint_item()];
    }
    items
}

/// Recently-used APPS, most-used first — the zero-state list.
///
/// `app_launcher` already records every launch under the app's lowercased
/// display name (`handlers/app_launcher.rs`), in a flat keyspace it shares with
/// `history:`, `win:`, `ws:`, `sug:` and bare file paths. There is no "this is
/// an app" marker, so the `AppIndex` lookup IS the test: a key that resolves to
/// an installed app is one, and a key that does not is skipped. An app since
/// uninstalled therefore disappears on its own.
///
/// The `run` is `open <Name>`, so Enter launches it — the same string
/// `AppLauncher::completions` emits, so selection behaves identically whether
/// the row came from here or from typing.
fn recent_apps(db: Option<&Arc<Database>>) -> Vec<CompletionItem> {
    let Some(db) = db else {
        return Vec::new();
    };
    let index = crate::desktop_apps::app_index();

    let mut scored: Vec<(&crate::desktop_apps::DesktopEntry, f64)> =
        frecency::get_scores_with_affinity(db)
            .into_iter()
            // Cheap pre-filter: everything namespaced (`history:`, `win:`,
            // `ws:`) or absolute is definitely not an app name. Correctness
            // still rests on the index lookup below; this only avoids probes.
            .filter(|(key, _)| !key.contains(':') && !key.starts_with('/'))
            .filter_map(|(key, score)| index.by_name_exact(&key).map(|e| (e, score)))
            .collect();

    // Score desc, then name for a deterministic order when scores tie.
    scored.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.name.cmp(&b.0.name))
    });

    scored
        .into_iter()
        // TAKE BEFORE RESOLVING ICONS. This ordering is the startup guard, not
        // a style choice: resolving every app's icon eagerly cost 6.5s of
        // warmup (see `handlers/app_launcher.rs` and the icon-warmup work), so
        // only the handful actually shown may pay it. Each `OnceLock` then
        // caches for the process lifetime.
        .take(MAX_RECENT_APPS)
        .map(|(entry, _)| {
            let icon_path = entry
                .icon_path
                .get_or_init(|| {
                    entry
                        .icon
                        .as_deref()
                        .and_then(crate::action_registry::handlers::icons::resolve_icon)
                })
                .clone();
            CompletionItem {
                label: entry.name.clone(),
                icon_path,
                score: 0,
                run: Some(format!("open {}", entry.name)),
                ..Default::default()
            }
        })
        .collect()
}

/// A canonical key for "what this command is really asking".
///
/// Lowercased, punctuation-stripped, deduplicated and SORTED words. Sorting
/// makes word order irrelevant; the set makes repetition irrelevant.
///
/// Deliberately NOT a stop-word list. That would collapse "can you define
/// gallop" onto "define gallop" too, but at the cost of a hand-maintained table
/// of meaningless words — English-only, and stale the moment it ships. What
/// this key does cover is the cheap, certain half: case, trailing punctuation,
/// and word order. Phrasings that differ by filler stay separate rows, and the
/// row cap keeps that bounded.
///
/// Edit distance was rejected outright: at any threshold loose enough to merge
/// "define gallop" with "can you define gallop", it also merges "define gallop"
/// with "define canter".
/// Kept, though nothing calls it since the zero state became apps-only.
///
/// `history:{cmd}` frecency keys are still WRITTEN on every execution
/// (`commands/execute.rs`), so the data a history surface would need is intact
/// — and this is the non-obvious part of reading it: four phrasings of one
/// question ("define gallop", "can you define gallop", …) are four keys and one
/// intent, and showing all four buries everything else. Deleting it would mean
/// rediscovering that. Its tests below still pin the behaviour.
#[allow(dead_code)]
fn intent_key(cmd: &str) -> String {
    let mut words: Vec<String> = cmd
        .split_whitespace()
        .map(|w| {
            w.trim_matches(|c: char| c.is_ascii_punctuation() && c != '-' && c != '_')
                .to_lowercase()
        })
        .filter(|w| !w.is_empty())
        .collect();
    words.sort();
    words.dedup();
    words.join(" ")
}

/// The empty-state hint shown to a brand-new user with no recents.
fn hint_item() -> CompletionItem {
    CompletionItem {
        label: "Type a command, or search the web".to_string(),
        icon_path: Some("__info__".to_string()),
        score: 0,
        description: Some("Your recent commands will appear here".to_string()),
        ..Default::default()
    }
}

/// Context actions matching the typed input — the home for context-derived
/// suggestions (git/docker/project/navigation), gated behind a matching
/// keyword. Called by the executor for input ≥ 2 chars. A candidate matches if
/// the typed needle is a substring of its command OR fuzzy-matches the
/// provider's domain keywords.
pub fn typed_matches(
    env: &EnvironmentContext,
    db: Option<&Arc<Database>>,
    input: &str,
) -> Vec<CompletionItem> {
    let needle = input.trim().to_lowercase();
    if needle.len() < 2 {
        return Vec::new();
    }
    let matching: Vec<(&'static str, Candidate)> = collect(env, db, ProviderTier::TypedOnly)
        .into_iter()
        .filter(|(provider, c)| candidate_matches(&needle, c, provider))
        .collect();

    let mut ranked = rank(matching, env, db);
    ranked.truncate(MAX_TYPED_MATCHES);
    ranked
        .into_iter()
        .map(|c| c.into_completion_item())
        .collect()
}

/// Whether a context candidate is a match for the typed needle: literal
/// substring of the command, or a fuzzy hit on its provider's keywords.
fn candidate_matches(needle: &str, c: &Candidate, provider_id: &str) -> bool {
    if c.command.to_lowercase().contains(needle) {
        return true;
    }
    keywords_for(provider_id)
        .iter()
        .any(|kw| fuzzy_subsequence(needle, kw))
}

// ── Pipeline ────────────────────────────────────────────────────────────

/// Run providers of the requested tier and collect candidates with provenance.
/// `tier` gates *which* providers run — cold-eligible (usage-driven/explicit)
/// vs typed-only (context actions shown after a matching keyword).
fn collect(
    env: &EnvironmentContext,
    db: Option<&Arc<Database>>,
    tier: ProviderTier,
) -> Vec<(&'static str, Candidate)> {
    let ctx = SuggestCtx {
        env,
        db,
        in_dev_window: env
            .active_window
            .as_ref()
            .is_some_and(|w| w.is_terminal || w.is_ide),
    };

    let mut out = Vec::new();
    for provider in providers() {
        if provider.tier() != tier {
            continue;
        }
        if !ctx.in_dev_window && !provider.universal() {
            continue;
        }
        for candidate in provider.suggest(&ctx) {
            out.push((provider.id(), candidate));
        }
    }
    out
}

/// The domain keywords a provider declares, looked up by its id.
fn keywords_for(provider_id: &str) -> &'static [&'static str] {
    providers()
        .iter()
        .find(|p| p.id() == provider_id)
        .map(|p| p.keywords())
        .unwrap_or(&[])
}

/// Case-insensitive subsequence match: is every char of `needle` found in
/// `haystack` in order? A cheap fuzzy test (no new dep) — "cont" ⊂ "container",
/// "dpnd" ⊂ "dependencies". Used to match typed input against provider
/// keywords for candidacy (never ranking).
fn fuzzy_subsequence(needle: &str, haystack: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    let mut chars = haystack.chars().flat_map(|c| c.to_lowercase());
    for want in needle.chars().flat_map(|c| c.to_lowercase()) {
        loop {
            match chars.next() {
                Some(h) if h == want => break,
                Some(_) => continue,
                None => return false,
            }
        }
    }
    true
}

/// Blend priors with learned acceptance, dedupe, gate, cap.
fn rank(
    candidates: Vec<(&'static str, Candidate)>,
    env: &EnvironmentContext,
    db: Option<&Arc<Database>>,
) -> Vec<Candidate> {
    let ctx_key = context_key(env);
    let learned = db
        .map(|db| frecency::get_suggestion_scores(db, &ctx_key))
        .unwrap_or_default();
    // Impression stats drive CTR demotion (self-tuning). (accepts, impressions)
    // per command; empty without a db → cold-start neutral for every candidate.
    let impressions = db
        .map(|db| frecency::get_impression_stats(db, &ctx_key))
        .unwrap_or_default();
    // Per-command circadian affinity for this workspace's remembered commands,
    // so cold-path workspace-memory suggestions get the same "knows your
    // routine" tiebreak the frecency recents already receive. Commands with no
    // workspace history (clipboard, typed-context) fall through to 1.0 neutral.
    let ws_affinity = db
        .zip(workspace_root(env))
        .map(|(db, root)| frecency::get_workspace_affinity(db, &root))
        .unwrap_or_default();

    // Score via the pure ranker: prior + learned boost, modulated by acceptance
    // CTR (demotes chronically-ignored) and time affinity. `Suppress` drops out.
    let mut scored: Vec<(&'static str, f64, Candidate)> = candidates
        .into_iter()
        .filter_map(|(provider, c)| {
            let boost = learned.get(&c.command).copied().unwrap_or(0.0) * LEARNED_BOOST_MAX;
            let (accepts, imps) = impressions.get(&c.command).copied().unwrap_or((0, 0));
            let affinity = ws_affinity.get(&c.command).copied().unwrap_or(1.0);
            match scoring::score_suggestion(c.relevance, boost, accepts, imps, affinity) {
                scoring::ScoredOutcome::Suppress => None,
                scoring::ScoredOutcome::Rank(score) if score >= CONFIDENCE_GATE as f64 => {
                    Some((provider, score, c))
                }
                scoring::ScoredOutcome::Rank(_) => None,
            }
        })
        .collect();

    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    // Dedupe by command (keep highest-scored) and cap per provider + total.
    let mut seen_commands: Vec<String> = Vec::new();
    let mut per_provider: std::collections::HashMap<&'static str, usize> =
        std::collections::HashMap::new();
    let mut out = Vec::new();
    for (provider, _score, candidate) in scored {
        if out.len() >= MAX_SUGGESTIONS {
            break;
        }
        if seen_commands.contains(&candidate.command) {
            continue;
        }
        let count = per_provider.entry(provider).or_insert(0);
        if *count >= MAX_PER_PROVIDER {
            continue;
        }
        *count += 1;
        seen_commands.push(candidate.command.clone());
        out.push(candidate);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::providers::Candidate;
    use super::*;

    #[test]
    fn case_punctuation_and_word_order_collapse() {
        // The cheap, certain half of dedup — no word list required.
        let k = intent_key("define gallop");
        for variant in [
            "define gallop?",
            "DEFINE GALLOP",
            "gallop define",
            "define  gallop",
        ] {
            assert_eq!(
                intent_key(variant),
                k,
                "{variant:?} should collapse onto {k:?}"
            );
        }
    }

    #[test]
    fn filler_words_do_not_collapse_and_that_is_deliberate() {
        // "can you define gallop" keeps its own key. Collapsing it would need a
        // stop-word list (hand-maintained, English-only) or edit distance —
        // which at any threshold loose enough to merge it with "define gallop"
        // also merges "define gallop" with "define canter". The row cap bounds
        // the cost instead.
        assert_ne!(
            intent_key("can you define gallop"),
            intent_key("define gallop")
        );
    }

    #[test]
    fn genuinely_different_commands_keep_distinct_keys() {
        // The failure mode to avoid: over-merging. Edit distance would risk
        // this; a word-set key doesn't.
        let keys = [
            intent_key("open spotify"),
            intent_key("open firefox"),
            intent_key("define gallop"),
            intent_key("weather in tokyo"),
            intent_key("weather in london"),
        ];
        let unique: std::collections::HashSet<_> = keys.iter().collect();
        assert_eq!(
            unique.len(),
            keys.len(),
            "distinct commands merged: {keys:?}"
        );
    }

    #[test]
    fn the_argument_still_distinguishes_otherwise_identical_commands() {
        // "define gallop" and "define canter" share a verb but ask different
        // things — they must remain separate rows.
        assert_ne!(intent_key("define gallop"), intent_key("define canter"));
    }

    fn cand(cmd: &str, relevance: u16) -> Candidate {
        Candidate {
            command: cmd.into(),
            description: cmd.into(),
            relevance,
            reason: SuggestionReason::GitDirty,
        }
    }

    pub(super) fn empty_env() -> EnvironmentContext {
        EnvironmentContext::default()
    }

    #[test]
    fn rank_gates_low_confidence() {
        let out = rank(
            vec![("git", cand("weak", 40)), ("git", cand("strong", 90))],
            &empty_env(),
            None,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].command, "strong");
    }

    #[test]
    fn rank_dedupes_and_caps() {
        let mut input = vec![("git", cand("dup", 90)), ("proj", cand("dup", 80))];
        for i in 0..20 {
            input.push(("mem", cand(&format!("cmd{i}"), 70)));
        }
        let out = rank(input, &empty_env(), None);
        assert!(out.len() <= MAX_SUGGESTIONS);
        assert_eq!(out.iter().filter(|c| c.command == "dup").count(), 1);
        // per-provider cap: at most 4 from "mem"
        assert!(out.iter().filter(|c| c.command.starts_with("cmd")).count() <= MAX_PER_PROVIDER);
    }

    #[test]
    fn context_key_prefers_project() {
        let mut env = empty_env();
        env.active_window = Some(super::super::WindowContext {
            title: "t".into(),
            wm_class: "firefox".into(),
            pid: 1,
            is_terminal: false,
            is_ide: false,
            window_id: None,
        });
        assert_eq!(context_key(&env), "app:firefox");
        env.project = Some(super::super::ProjectContext {
            root: "/home/u/proj/".into(),
            kind: super::super::ProjectKind::Rust,
            has_compose: false,
            scripts: vec![],
            package_manager: None,
            workspace_root: None,
            workspace_scripts: vec![],
        });
        assert_eq!(context_key(&env), "proj:/home/u/proj");
    }

    // ── End-to-end engine behavior ──────────────────────────────────────

    pub(super) fn dev_env() -> EnvironmentContext {
        let mut env = empty_env();
        env.active_window = Some(super::super::WindowContext {
            title: "term".into(),
            wm_class: "org.gnome.terminal".into(),
            pid: 1,
            is_terminal: true,
            is_ide: false,
            window_id: None,
        });
        env.git = Some(super::super::GitContext {
            repo_root: "/home/u/proj".into(),
            branch: "main".into(),
            dirty: true,
            remote: None,
        });
        env
    }

    /// A multi-repo workspace: `git` is None, `repos` carries the siblings.
    /// This is the shape the model exists to represent — the container is not a
    /// repo, so there is no single unambiguous target.

    #[test]
    fn typing_a_command_never_proposes_a_different_one() {
        // The contract this module now holds: suggestions never invent a
        // command the user didn't type. Git/project/docker verb-guessing used
        // to answer "git" with `git pull`/`git push` — commands nobody asked
        // for, which buried real results and still missed the actual intent.
        //
        // Where the command should RUN is a separate question, answered by
        // `Executor::multi_repo_rows` once a command exists to place.
        let env = dev_env();
        for query in ["git", "git status", "npm", "docker"] {
            let items = typed_matches(&env, None, query);
            for item in &items {
                let run = item.run.as_deref().unwrap_or(&item.label);
                assert!(
                    run.to_lowercase().contains(&query.to_lowercase()),
                    "query {query:?} produced unrelated suggestion {run:?}"
                );
            }
        }
    }

    #[test]
    fn zero_state_never_shows_speculative_context() {
        // Dirty git repo, terminal focused, but NO history/recents. The
        // zero-state must NOT surface git commit/diff/etc. — context actions
        // are typed-gated now. Only the hint should show.
        let items = suggest(&dev_env(), None);
        assert!(
            !items.iter().any(|i| i.label.starts_with("git ")),
            "zero-state must not show speculative git commands"
        );
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].icon_path.as_deref(), Some("__info__"));
    }

    #[test]
    fn zero_state_never_shows_speculative_commands() {
        // Rewritten 2026-08-07: the zero state is APPS now, so the original
        // assertions (history rows labelled "open firefox" with a
        // `__history__` clock) describe behaviour that was deliberately
        // removed — see `recent_apps`. What this test still owns, and what it
        // was really guarding, is that a dirty repo does NOT conjure `git`
        // commands nobody ran.
        let db = crate::db::open_test_database();
        frecency::record(&db, "history:open firefox").unwrap();
        frecency::record(&db, "history:web rust docs").unwrap();
        let items = suggest(&dev_env(), Some(&db));
        assert!(
            !items.iter().any(|i| i.label.starts_with("git ")),
            "speculative git commands must never appear: {items:?}"
        );
        // And history keys must not come back as raw command text.
        assert!(
            !items.iter().any(|i| i.label == "web rust docs"),
            "a raw command string leaked into the app zero state: {items:?}"
        );
    }

    #[test]
    fn zero_state_hint_for_new_user() {
        // Empty store, empty context → a single honest hint, never junk.
        let items = suggest(&empty_env(), None);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].icon_path.as_deref(), Some("__info__"));
    }

    #[test]
    fn suggest_non_dev_window_skips_dev_providers() {
        let mut env = dev_env();
        // Browser focused: git context exists but window isn't a dev window.
        env.active_window.as_mut().unwrap().is_terminal = false;
        let items = suggest(&env, None);
        assert!(
            !items.iter().any(|i| i.label.starts_with("git ")),
            "git suggestions must not appear outside dev windows"
        );
    }

    #[test]
    fn suggest_clipboard_error_carries_real_query() {
        let mut env = empty_env();
        env.clipboard = Some(
            super::super::clipboard_detect::ClipboardContentType::ErrorTrace(
                "TypeError: x is undefined".into(),
            ),
        );
        let items = suggest(&env, None);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "web TypeError: x is undefined");
    }

    #[test]
    fn fuzzy_subsequence_matches_in_order() {
        assert!(fuzzy_subsequence("cont", "container"));
        assert!(fuzzy_subsequence("dpnd", "dependencies"));
        assert!(!fuzzy_subsequence("xyz", "container"));
        assert!(!fuzzy_subsequence("tac", "cat")); // order matters
        assert!(fuzzy_subsequence("", "anything"));
    }

    #[test]
    fn workspace_root_derives_from_project_then_cwd() {
        // The affinity lookup in rank() must key on the SAME root MemoryProvider
        // stores under: project root first, else cwd.
        let mut env = empty_env();
        env.cwd = Some("/home/u/loose".into());
        assert_eq!(workspace_root(&env).as_deref(), Some("/home/u/loose"));
        env.project = Some(super::super::ProjectContext {
            root: "/home/u/proj".into(),
            kind: super::super::ProjectKind::Rust,
            has_compose: false,
            scripts: vec![],
            package_manager: None,
            workspace_root: None,
            workspace_scripts: vec![],
        });
        assert_eq!(workspace_root(&env).as_deref(), Some("/home/u/proj"));
    }
}

#[cfg(test)]
mod app_zero_state_tests {
    use super::*;
    use crate::db::frecency;
    use crate::desktop_apps::index;

    /// Pin the global app index for the duration of a test.
    ///
    /// The index is process-wide, so these tests must serialise on its lock
    /// and restore the real index afterwards — otherwise they assert whatever
    /// the machine happens to have installed, which passes on a developer
    /// desktop and fails on a bare CI runner.
    fn with_apps(names: &[(&str, &str)]) -> impl Drop {
        struct Restore {
            _lock: std::sync::MutexGuard<'static, ()>,
        }
        impl Drop for Restore {
            fn drop(&mut self) {
                index::rebuild_app_index();
            }
        }
        let guard = index::test_index_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        index::set_app_index_for_test(
            names
                .iter()
                .map(|(name, exec)| {
                    index::tests::make_entry(name, exec, &[], None, Some(&name.to_lowercase()))
                })
                .collect(),
        );
        Restore { _lock: guard }
    }

    fn env() -> EnvironmentContext {
        super::tests::empty_env()
    }

    /// The headline: a launched app comes back as an APP ROW, not the literal
    /// text of the command that launched it.
    #[test]
    fn a_launched_app_returns_as_an_app_row() {
        let _idx = with_apps(&[("Spotify", "/usr/bin/spotify")]);
        let db = crate::db::open_test_database();
        // Exactly what `app_launcher` records on launch.
        frecency::record(&db, "spotify").unwrap();

        let items = suggest(&env(), Some(&db));

        let row = items
            .iter()
            .find(|i| i.label == "Spotify")
            .unwrap_or_else(|| panic!("no Spotify app row: {items:?}"));
        assert_eq!(row.run.as_deref(), Some("open Spotify"));
        assert!(
            !items.iter().any(|i| i.label == "open spotify"),
            "the raw command string is still being shown: {items:?}"
        );
    }

    /// Launching an app writes BOTH `spotify` and `history:open spotify`, so
    /// without run-keyed dedupe the user would see the app row AND the literal
    /// text — the exact bug this change exists to remove.
    #[test]
    fn the_history_twin_of_an_app_never_appears() {
        let _idx = with_apps(&[("Spotify", "/usr/bin/spotify")]);
        let db = crate::db::open_test_database();
        frecency::record(&db, "spotify").unwrap();
        frecency::record(&db, "history:open spotify").unwrap();

        let items = suggest(&env(), Some(&db));
        let opens: Vec<&CompletionItem> = items
            .iter()
            .filter(|i| {
                i.run
                    .as_deref()
                    .unwrap_or(&i.label)
                    .to_lowercase()
                    .starts_with("open spotify")
            })
            .collect();
        assert_eq!(
            opens.len(),
            1,
            "expected exactly one Spotify row: {items:?}"
        );
        assert_eq!(opens[0].label, "Spotify");
    }

    /// The keyspace is flat and shared. Only keys that resolve to an installed
    /// app may become rows.
    #[test]
    fn non_app_frecency_keys_never_become_rows() {
        let _idx = with_apps(&[("Spotify", "/usr/bin/spotify")]);
        let db = crate::db::open_test_database();
        for key in [
            "history:web rust docs",
            "win:firefox",
            "ws:/home/u/p:cargo test",
            "/home/u/notes.md",
            "sug:something",
        ] {
            frecency::record(&db, key).unwrap();
        }

        let items = suggest(&env(), Some(&db));

        // Assert on the COUNT, not on the labels. Checking labels only proves
        // the key text did not leak verbatim — a bug that resolved every key
        // to some arbitrary app would produce perfectly clean-looking labels
        // and still be wrong. Five non-app keys were recorded and no app key
        // was, so the only correct answer is zero app rows.
        assert!(
            items
                .iter()
                .all(|i| i.run.is_none()
                    || !i.run.as_deref().unwrap_or_default().starts_with("open ")),
            "a non-app frecency key produced an app row: {items:?}"
        );
    }

    /// An app recorded once and since uninstalled must not linger as a dead
    /// row that launches nothing.
    #[test]
    fn an_uninstalled_app_is_dropped() {
        let _idx = with_apps(&[("Spotify", "/usr/bin/spotify")]);
        let db = crate::db::open_test_database();
        frecency::record(&db, "spotify").unwrap();
        frecency::record(&db, "an-app-that-was-removed").unwrap();

        let items = suggest(&env(), Some(&db));
        assert!(
            !items
                .iter()
                .any(|i| i.label.eq_ignore_ascii_case("an-app-that-was-removed")),
            "an uninstalled app is still listed: {items:?}"
        );
    }

    #[test]
    fn app_rows_respect_the_cap() {
        let apps: Vec<(String, String)> = (0..10)
            .map(|i| (format!("App{i}"), format!("/usr/bin/app{i}")))
            .collect();
        let refs: Vec<(&str, &str)> = apps.iter().map(|(n, e)| (n.as_str(), e.as_str())).collect();
        let _idx = with_apps(&refs);

        let db = crate::db::open_test_database();
        for (name, _) in &apps {
            frecency::record(&db, &name.to_lowercase()).unwrap();
        }

        let items = suggest(&env(), Some(&db));
        assert!(
            items.len() <= MAX_COLD_RECENTS,
            "over the total cap: {items:?}"
        );
        let app_rows = items.iter().filter(|i| i.label.starts_with("App")).count();
        assert!(
            app_rows <= MAX_RECENT_APPS,
            "expected <= {MAX_RECENT_APPS} app rows, got {app_rows}"
        );
    }

    /// The startup guard, enforced rather than commented. Resolving every
    /// app's icon eagerly cost 6.5s of warmup; only the rows actually shown
    /// may pay for one.
    #[test]
    fn only_the_shown_rows_resolve_an_icon() {
        let apps: Vec<(String, String)> = (0..10)
            .map(|i| (format!("App{i}"), format!("/usr/bin/app{i}")))
            .collect();
        let refs: Vec<(&str, &str)> = apps.iter().map(|(n, e)| (n.as_str(), e.as_str())).collect();
        let _idx = with_apps(&refs);

        let db = crate::db::open_test_database();
        for (name, _) in &apps {
            frecency::record(&db, &name.to_lowercase()).unwrap();
        }

        let _ = suggest(&env(), Some(&db));

        let resolved = index::app_index()
            .entries
            .iter()
            .filter(|e| e.icon_path.get().is_some())
            .count();
        assert!(
            resolved <= MAX_RECENT_APPS,
            "resolved {resolved} icons for at most {MAX_RECENT_APPS} visible rows — \
             the take-before-resolve ordering has been lost"
        );
    }

    /// The screenshot bug: workspace memory offered `open Xfce Terminal` as a
    /// raw command row (lightning bolt, "Recent in <project>") ABOVE the real
    /// Xfce Terminal app row. Two rows, one target, and the uglier one first.
    #[test]
    fn workspace_memory_does_not_duplicate_an_app_row() {
        let _idx = with_apps(&[("Xfce Terminal", "/usr/bin/xfce4-terminal")]);
        let db = crate::db::open_test_database();
        // The app itself, and a workspace memory of having launched it here.
        frecency::record(&db, "xfce terminal").unwrap();
        frecency::record_workspace(&db, "/home/u/proj", "open Xfce Terminal").unwrap();

        // `MemoryProvider` keys on the project root or, failing that, the cwd.
        let mut env = super::tests::dev_env();
        env.cwd = Some("/home/u/proj".into());

        let items = suggest(&env, Some(&db));
        let raw = items
            .iter()
            .filter(|i| i.label.starts_with("open "))
            .count();
        assert_eq!(
            raw, 0,
            "an app launch is still shown as raw command text: {items:?}"
        );
    }

    /// ...but workspace memory must keep offering real COMMANDS. Dropping app
    /// launches must not gut the feature.
    #[test]
    fn workspace_memory_still_offers_commands() {
        let _idx = with_apps(&[("Xfce Terminal", "/usr/bin/xfce4-terminal")]);
        let db = crate::db::open_test_database();
        frecency::record_workspace(&db, "/home/u/proj", "cargo test").unwrap();

        let mut env = super::tests::dev_env();
        env.cwd = Some("/home/u/proj".into());

        let items = suggest(&env, Some(&db));
        assert!(
            items.iter().any(|i| i.label == "cargo test"),
            "workspace memory stopped offering commands: {items:?}"
        );
    }

    /// A brand-new user sees the honest hint, not an empty list.
    #[test]
    fn no_frecency_yields_the_hint() {
        let _idx = with_apps(&[("Spotify", "/usr/bin/spotify")]);
        let db = crate::db::open_test_database();
        let items = suggest(&env(), Some(&db));
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].icon_path.as_deref(), Some("__info__"));
    }
}

#[cfg(test)]
mod app_lookup_boundary {
    use super::*;
    use crate::db::frecency;
    use crate::desktop_apps::index;

    /// The index lookup — not the `:`/`/` pre-filter — is what decides whether
    /// a frecency key names an app. This pins that directly: a key that is NOT
    /// an app name must produce no row even though it passes the pre-filter.
    #[test]
    fn a_plain_non_app_key_produces_no_row() {
        let _g = index::test_index_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        index::set_app_index_for_test(vec![index::tests::make_entry(
            "Spotify",
            "/usr/bin/spotify",
            &[],
            None,
            Some("spotify"),
        )]);

        let db = crate::db::open_test_database();
        // No colon, no leading slash — sails past the pre-filter, and is still
        // not an installed app.
        frecency::record(&db, "definitely-not-an-app").unwrap();

        let items = suggest(&super::tests::empty_env(), Some(&db));
        assert!(
            !items.iter().any(|i| i.run.is_some()),
            "a key that is not an app produced a launchable row: {items:?}"
        );

        index::rebuild_app_index();
    }
}

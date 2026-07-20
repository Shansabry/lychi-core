use lychi_core::action_registry::CompletionItem;
use lychi_core::error::LychiError;
use lychi_core::file_search::{
    self, DirEntry, FileSearchBatch, FileSearchResult, MountPoint, build_result_modified,
    finalize_row, frecency_recency_bonus, search_display_label, section_header,
};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use ignore::WalkBuilder;
use nucleo_matcher::pattern::{Atom, AtomKind, CaseMatching, Normalization};
use nucleo_matcher::{Config as MatcherConfig, Matcher, Utf32Str};
use tauri::{AppHandle, Emitter, State};

use crate::state::AppState;

/// Given the text after `@`, return filesystem completions.
///
/// - Empty or `~` → list home directory
/// - Bare name (e.g. `Do`) → filter home directory contents
/// - `/...` → absolute path
/// - Directories sort before files, max 10 results
#[tauri::command]
#[specta::specta]
pub async fn list_path_completions(partial: String) -> Result<Vec<CompletionItem>, LychiError> {
    tauri::async_runtime::spawn_blocking(move || file_search::list_path_completions_sync(partial))
        .await
        .map_err(|e| LychiError::ExecutionFailed(format!("path completions task panicked: {e}")))?
}

/// List subdirectories of the given path (directories only, absolute paths).
/// Used by the in-app folder picker to avoid the native GTK dialog which
/// crashes on Wayland layer-shell surfaces.
#[tauri::command]
#[specta::specta]
pub async fn list_directories(path: String) -> Result<Vec<DirEntry>, LychiError> {
    tauri::async_runtime::spawn_blocking(move || file_search::list_directories_sync(path))
        .await
        .map_err(|e| LychiError::ExecutionFailed(format!("list_directories task panicked: {e}")))?
}

/// Detect real mounted filesystems. Home directory is always first.
#[tauri::command]
#[specta::specta]
pub async fn get_mount_points() -> Result<Vec<MountPoint>, LychiError> {
    tauri::async_runtime::spawn_blocking(file_search::get_mount_points_sync)
        .await
        .map_err(|e| LychiError::ExecutionFailed(format!("get_mount_points task panicked: {e}")))?
}

/// Start a file search. An empty query is a directory *listing* (fast depth-1
/// readdir, unchanged). A non-empty query is a *fuzzy search* served by the
/// persistent per-scope nucleo index — instant per keystroke, no re-walk.
#[tauri::command]
#[specta::specta]
pub async fn start_file_search(
    query: String,
    scope: String,
    search_id: u64,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), LychiError> {
    state.active_file_search.store(search_id, Ordering::SeqCst);
    let active_id = state.active_file_search.clone();
    let db = state.db.clone();

    // Empty query → directory listing. Keep the direct depth-1 walk (no index
    // needed; listing should show everything including ignored folders).
    if query.trim().is_empty() {
        tauri::async_runtime::spawn_blocking(move || {
            walk_and_emit(&query, &scope, search_id, &active_id, &db, &app);
        });
        return Ok(());
    }

    // Fuzzy search via the persistent index. Get/build the scope's engine, then
    // match. `notify` re-runs the search when more of the index streams in, so
    // results fill as the background walk progresses.
    let store = state.file_index.clone();
    let app_notify = app.clone();
    let scope_notify = scope.clone();
    let query_notify = query.clone();
    let db_notify = db.clone();
    let active_notify = active_id.clone();
    let store_notify = store.clone();
    let notify: std::sync::Arc<dyn Fn() + Send + Sync> = std::sync::Arc::new(move || {
        // Only re-emit if this scope's search is still the active one.
        if active_notify.load(Ordering::Relaxed) == search_id {
            emit_index_results(
                &store_notify,
                &scope_notify,
                &query_notify,
                search_id,
                &db_notify,
                &app_notify,
            );
        }
    });

    tauri::async_runtime::spawn_blocking(move || {
        let index = store.get_or_build(&scope, notify);
        // Run the match, then emit whatever's matched so far. The notify
        // callback handles later fills as indexing completes.
        if let Ok(mut idx) = index.lock() {
            idx.search(&query, 10);
        }
        emit_index_results(&store, &scope, &query, search_id, &db, &app);
    });

    Ok(())
}

/// Read the current matches from the scope's index, RE-RANK them with explicit
/// match tiers (Spotlight/Raycast/fzf model — see `lychi_core::file_search_score`),
/// split into Folders and Files groups, and emit with section-header rows.
///
/// nucleo is used only as a fast candidate *generator* (its parallel walk +
/// fuzzy filter narrows millions of paths to the matching set); ranking is ours,
/// because nucleo's path-scheme score ties a folder with its own children.
fn emit_index_results(
    store: &Arc<lychi_core::file_search::FileIndexStore>,
    scope: &str,
    query: &str,
    search_id: u64,
    db: &Arc<redb::Database>,
    app: &AppHandle,
) {
    use lychi_core::file_search_score::{MatchScore, classify};

    // Per group; folders and files are ranked independently, so each can fill
    // its own section without one starving the other.
    const PER_GROUP: usize = 25;
    // Over-fetch from nucleo: it fuzzy-filtered already, so this pool is "paths
    // that match at all". We re-rank the whole pool, then take the best per group.
    const CANDIDATE_POOL: usize = 400;

    let (items, done) = {
        let Some(index) = store.peek(scope) else {
            return;
        };
        let Ok(mut idx) = index.lock() else {
            return;
        };
        // Pull any newly-injected/matched items into the snapshot before reading.
        idx.refresh(10);
        (idx.top(CANDIDATE_POOL), idx.is_complete())
    };

    let frecency_scores = lychi_core::db::frecency::get_scores(db);
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let home = dirs::home_dir();

    // Classify every candidate into a match tier + in-tier signals. Anything the
    // scorer rejects (query hits neither name nor any ancestor dir) is dropped —
    // an extra guard on top of nucleo's own filter.
    struct Row {
        score: MatchScore,
        bonus: u16,
        is_dir: bool,
        result: FileSearchResult,
    }
    let mut rows: Vec<Row> = items
        .into_iter()
        .filter_map(|d| {
            let score = classify(query, &d.file_name, &d.rel_path)?;
            let bonus = frecency_recency_bonus(
                frecency_scores.get(&d.full_path).copied(),
                d.modified_secs,
                now_secs,
            );
            let path = Path::new(&d.full_path);
            let description = if d.is_dir {
                Some("Folder".to_string())
            } else {
                d.file_name
                    .rsplit('.')
                    .next()
                    .filter(|ext| !ext.is_empty() && ext.len() < 6 && *ext != d.file_name)
                    .map(|ext| ext.to_uppercase())
            };
            Some(Row {
                score,
                bonus,
                is_dir: d.is_dir,
                result: FileSearchResult {
                    label: search_display_label(path, d.is_dir, home.as_deref()),
                    full_path: d.full_path.clone(),
                    is_dir: d.is_dir,
                    score: 0, // set below from final display rank
                    description,
                    size_bytes: d.size_bytes,
                    modified_secs: d.modified_secs,
                },
            })
        })
        .collect();

    // Rank: tier (best first) → shorter filename → shallower path → more used.
    // This is fzf's `--tiebreak=pathname,length` plus a Raycast usage layer, but
    // gated under a discrete tier so a real filename match always beats a
    // path-only one (the parent-vs-children fix is structural, not a tiebreak).
    let rank = |a: &Row, b: &Row| {
        a.score
            .tier
            .cmp(&b.score.tier) // lower tier value = better match
            .then_with(|| a.score.name_len.cmp(&b.score.name_len)) // shorter name first
            .then_with(|| a.score.depth.cmp(&b.score.depth)) // shallower first
            .then_with(|| b.bonus.cmp(&a.bonus)) // more used first
            .then_with(|| a.result.label.cmp(&b.result.label)) // stable
    };

    let (mut folders, mut files): (Vec<Row>, Vec<Row>) = rows.drain(..).partition(|r| r.is_dir);
    folders.sort_by(&rank);
    files.sort_by(&rank);
    folders.truncate(PER_GROUP);
    files.truncate(PER_GROUP);

    // Build the emitted list: a "folders" section header, the folders, then a
    // "files" section header, then the files. Headers use the `__separator__`
    // sentinel the CompletionsList already renders (centered label between
    // lines) — no new UI. A section is omitted entirely when it has no matches,
    // so an all-files or all-folders query shows just the one group.
    //
    // Display score is a single descending integer so the frontend's own
    // score-sort preserves exactly this order (headers get a score above their
    // section's items but below the previous section's, keeping them in place).
    let mut out: Vec<FileSearchResult> = Vec::with_capacity(folders.len() + files.len() + 2);
    let mut next_score: u16 = (folders.len() + files.len() + 4) as u16;

    if !folders.is_empty() {
        out.push(section_header("folders", &mut next_score));
        for row in folders {
            out.push(finalize_row(row.result, &mut next_score));
        }
    }
    if !files.is_empty() {
        out.push(section_header("files", &mut next_score));
        for row in files {
            out.push(finalize_row(row.result, &mut next_score));
        }
    }

    let _ = app.emit(
        "lychi://file-search-results",
        FileSearchBatch {
            search_id,
            results: out,
            done,
            has_ignore_rules: false,
        },
    );
}

/// Cancel any in-flight file search.
#[tauri::command]
#[specta::specta]
pub async fn cancel_file_search(state: State<'_, AppState>) -> Result<(), LychiError> {
    state.active_file_search.store(0, Ordering::SeqCst);
    Ok(())
}

fn walk_and_emit(
    query: &str,
    scope: &str,
    search_id: u64,
    active_id: &Arc<std::sync::atomic::AtomicU64>,
    db: &Arc<redb::Database>,
    app: &AppHandle,
) {
    const BATCH_SIZE: usize = 10;
    const MAX_RESULTS: usize = 50;

    let is_listing = query.is_empty();

    // Usage-driven ranking signal: files the user has actually opened (recorded
    // by the `file` handler under their absolute path) get a frecency bonus, so
    // a familiar file outranks a fuzzy-equal stranger — the Raycast/Alfred
    // standard. Fetched once per search (all keys); file paths are absolute so
    // they don't collide with the prefixed keys (history:/ws:/sug:).
    let frecency_scores = lychi_core::db::frecency::get_scores(db);
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let wants_hidden = query.starts_with('.');
    let query_has_slash = query.contains('/');
    let home = dirs::home_dir();

    let mut matcher = Matcher::new(MatcherConfig::DEFAULT);
    let pattern = Atom::new(
        query,
        CaseMatching::Ignore,
        Normalization::Smart,
        AtomKind::Fuzzy,
        false,
    );

    const MAX_SEARCH_DEPTH: usize = 10;

    let scope_path = Path::new(scope);
    let has_ignore_rules =
        scope_path.join(".gitignore").exists() || scope_path.join(".ignore").exists();

    let mut builder = WalkBuilder::new(scope);
    builder
        .hidden(!wants_hidden)
        .ignore(!is_listing) // Don't apply .ignore rules when listing — they hide folders silently
        .git_ignore(!is_listing) // Only apply .gitignore during search, not directory listing
        .git_global(false)
        .git_exclude(false)
        .follow_links(false)
        .max_depth(Some(if is_listing { 1 } else { MAX_SEARCH_DEPTH }))
        // Honor `.gitignore` / `.ignore` even outside a git repo. The `ignore`
        // crate normally only applies `.gitignore` inside an actual git repo, so
        // a project with a `.gitignore` (e.g. listing `node_modules/`) but no
        // `.git` — common for vendored or copied project trees — would flood
        // results with dependency trees, burying real matches. `require_git(false)`
        // makes the project's OWN ignore rules apply regardless of git status:
        // fully adaptive to whatever each folder declares, zero hardcoded names.
        // Only during search — listing a directory should still show everything.
        .require_git(is_listing);
    let walker = builder.build();

    // Helper: build a result from a walk entry
    let build_result = |entry: &ignore::DirEntry,
                        file_name: &str,
                        is_dir: bool,
                        score: u16,
                        home: Option<&Path>|
     -> FileSearchResult {
        let path = entry.path();
        let meta = entry.metadata().ok();
        let size_bytes = meta.as_ref().map(|m| m.len());
        let modified_secs = meta
            .as_ref()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs());
        let description = if is_dir {
            Some("Folder".into())
        } else {
            file_name
                .rsplit('.')
                .next()
                .filter(|ext| !ext.is_empty() && ext.len() < 6 && *ext != file_name)
                .map(|ext| ext.to_uppercase())
        };
        FileSearchResult {
            label: search_display_label(path, is_dir, home),
            full_path: path.to_string_lossy().to_string(),
            is_dir,
            score,
            description,
            size_bytes,
            modified_secs,
        }
    };

    if is_listing {
        // Directory listing: collect ALL items, sort globally (dirs first, then alpha), emit once.
        // With max_depth=1 this is just a readdir — fast even for 500+ items.
        let mut all: Vec<FileSearchResult> = Vec::new();

        for entry in walker {
            if active_id.load(Ordering::Relaxed) != search_id {
                return;
            }
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            let file_name = match entry.file_name().to_str() {
                Some(n) => n,
                None => continue,
            };
            let is_dir = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
            if entry.depth() == 0 {
                continue;
            }
            if file_name.starts_with('.') && !wants_hidden {
                continue;
            }
            let score = if is_dir { 100 } else { 50 };
            all.push(build_result(
                &entry,
                file_name,
                is_dir,
                score,
                home.as_deref(),
            ));
        }

        // Sort: dirs first, then alphabetically by label within each group
        all.sort_by(|a, b| {
            b.is_dir
                .cmp(&a.is_dir)
                .then_with(|| a.label.to_lowercase().cmp(&b.label.to_lowercase()))
        });
        all.truncate(MAX_RESULTS);

        let _ = app.emit(
            "lychi://file-search-results",
            FileSearchBatch {
                search_id,
                results: all,
                done: true,
                has_ignore_rules,
            },
        );
        return;
    }

    // Fuzzy search: gather all matches, globally sort at the end. A fast first
    // preview batch keeps the UI responsive without sacrificing final ordering.
    let mut matches: Vec<FileSearchResult> = Vec::with_capacity(BATCH_SIZE * 4);
    let mut sent_first = false;

    for entry in walker {
        if active_id.load(Ordering::Relaxed) != search_id {
            return;
        }

        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };

        let file_name = match entry.file_name().to_str() {
            Some(n) => n,
            None => continue,
        };

        let is_dir = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);

        if file_name.starts_with('.') && !wants_hidden {
            continue;
        }

        let path = entry.path();

        let mut buf = Vec::new();
        // Try filename match first — boosted so name hits rank above path-only hits
        let name_haystack = Utf32Str::new(file_name, &mut buf);
        let name_score = pattern.score(name_haystack, &mut matcher);

        // Also try path match for queries with '/'
        let rel = path.strip_prefix(scope).unwrap_or(path).to_string_lossy();
        buf.clear();
        let path_haystack = Utf32Str::new(&rel, &mut buf);
        let path_score = pattern.score(path_haystack, &mut matcher);

        let match_score = match (name_score, path_score) {
            (Some(ns), _) => ns.saturating_add(100), // filename match: boosted
            (None, Some(ps)) if query_has_slash => ps, // path match: only when query has '/'
            _ => continue,
        };

        // Blend in usage frecency + recency so a file the user opens often (or
        // touched recently) outranks a fuzzy-equal stranger.
        let full_path = path.to_string_lossy();
        let bonus = frecency_recency_bonus(
            frecency_scores.get(full_path.as_ref()).copied(),
            build_result_modified(&entry),
            now_secs,
        );
        let score = match_score.saturating_add(bonus);

        matches.push(build_result(
            &entry,
            file_name,
            is_dir,
            score,
            home.as_deref(),
        ));

        // Stream a fast FIRST batch for perceived responsiveness: emit the top
        // few as soon as we have them, so the user sees results immediately.
        // The final emit re-sends the FULL globally-sorted list, so any better
        // match found later lands in the right position (fixes the per-batch
        // ordering artifact).
        if !sent_first && matches.len() >= 3 {
            let mut preview = matches.clone();
            preview.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.label.cmp(&b.label)));
            preview.truncate(3);
            let _ = app.emit(
                "lychi://file-search-results",
                FileSearchBatch {
                    search_id,
                    results: preview,
                    done: false,
                    has_ignore_rules,
                },
            );
            sent_first = true;
        }
        if matches.len() >= MAX_RESULTS * 4 {
            // Enough candidates gathered to pick a stable top-N; stop walking.
            break;
        }
    }

    // Global sort: score desc, then label asc as a stable tie-breaker so
    // equal-score results have a deterministic order (not walk order).
    matches.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.label.cmp(&b.label)));
    matches.truncate(MAX_RESULTS);

    let _ = app.emit(
        "lychi://file-search-results",
        FileSearchBatch {
            search_id,
            results: matches,
            done: true,
            has_ignore_rules,
        },
    );
}

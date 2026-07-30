use lychi_core::action_registry::CompletionItem;
use lychi_core::error::LychiError;
use lychi_core::file_search::{
    self, DirEntry, FileSearchBatch, FileSearchResult, MountPoint, build_result_modified,
    finalize_row, frecency_recency_bonus, search_display_label, section_header,
};
use lychi_core::files::attachment::{self, FileAttachment};
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

/// Fuzzy jump-to-file for the `@` reference (Claude-Code-style). Given a bare
/// query (no path separators), returns the best matches ANYWHERE under home,
/// ranked by the same tier + frecency blend the `/` search uses. Reuses the
/// warm recursive index rather than listing a single directory.
#[tauri::command]
#[specta::specta]
pub async fn fuzzy_path_completions(
    query: String,
    state: State<'_, AppState>,
) -> Result<Vec<CompletionItem>, LychiError> {
    let live = state.live_search.clone();
    let db = state.db.clone();
    let scope = dirs::home_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| "/".to_string());
    tauri::async_runtime::spawn_blocking(move || {
        file_search::fuzzy_path_completions(&live, &scope, &query, &db, 15)
    })
    .await
    .map_err(|e| LychiError::ExecutionFailed(format!("fuzzy completions task panicked: {e}")))
}

/// Classify user-supplied file paths into chip-ready attachments (kind, mime,
/// thumbnail, and which backend pipe they'll take). The frontend calls this once
/// per attach gesture and renders the result; it never classifies files itself.
#[tauri::command]
#[specta::specta]
pub async fn classify_files(paths: Vec<String>) -> Result<Vec<FileAttachment>, LychiError> {
    // Thumbnail encoding decodes images — keep it off the async runtime.
    tauri::async_runtime::spawn_blocking(move || attachment::classify_attachments(&paths))
        .await
        .map_err(|e| LychiError::ExecutionFailed(format!("classify_files task panicked: {e}")))
}

/// Stage whatever the clipboard holds as attachments (the paste gesture).
///
/// Two shapes, in order: copied FILES (a file manager's `file://` URI list —
/// the bytes are already on disk, so we just take the paths), or copied IMAGE
/// DATA (a screenshot tool / "copy image" — no path exists, so we spill it to
/// the clipboard-images dir first and attach that). Copied text is deliberately
/// ignored: it belongs in the input box, not the attachment tray.
///
/// Returns chips via the SAME classifier as every other attach gesture
/// (`files::attachment`), so a pasted file and a picked file behave identically.
#[tauri::command]
#[specta::specta]
pub async fn attach_from_clipboard() -> Result<Vec<FileAttachment>, LychiError> {
    let is_wayland = lychi_core::context::is_wayland();
    tauri::async_runtime::spawn_blocking(move || {
        // 1. Copied files — the common case (Ctrl+C in Nautilus/Dolphin).
        let paths = lychi_core::clipboard::files::read_clipboard_files(is_wayland);
        if !paths.is_empty() {
            let strs: Vec<String> = paths
                .iter()
                .map(|p| p.to_string_lossy().to_string())
                .collect();
            return attachment::classify_attachments(&strs);
        }

        // 2. Copied image DATA — persist it so it has a path to attach.
        if let Some(path) = lychi_core::clipboard::image_utils::spill_clipboard_image(is_wayland) {
            return attachment::classify_attachments(&[path]);
        }

        Vec::new()
    })
    .await
    .map_err(|e| LychiError::ExecutionFailed(format!("clipboard attach task panicked: {e}")))
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

    // Fuzzy search. `LiveSearch` owns the session and the generation together, so
    // starting this search cancels the previous one by dropping its matcher —
    // there is no in-flight query left to race with.
    let live = state.live_search.clone();

    tauri::async_runtime::spawn_blocking(move || {
        // nucleo calls the redraw hook from its own threadpool as more results
        // score, and it needs the generation to ask for results — which `begin`
        // only returns once it has been given the hook. A shared cell closes that
        // cycle: written once here, read on every later redraw.
        let generation_cell = Arc::new(std::sync::OnceLock::new());

        let redraw: Arc<dyn Fn() + Send + Sync> = {
            let live = live.clone();
            let cell = generation_cell.clone();
            let db = db.clone();
            let app = app.clone();
            Arc::new(move || {
                // Before the cell is set, the initial emit below hasn't run yet
                // and will cover this. Afterwards, `emit_ranked_results` drops
                // the batch itself if the generation has been superseded.
                if let Some(&generation) = cell.get() {
                    emit_ranked_results(&live, generation, &db, &app, search_id);
                }
            })
        };

        let generation = live.begin(&scope, &query, redraw);
        let _ = generation_cell.set(generation);
        emit_ranked_results(&live, generation, &db, &app, search_id);
    });

    Ok(())
}

/// Rank the current matches and emit one batch to the frontend.
///
/// Ranking is `lychi_core::file_search::rank` — the SAME function the `@`
/// reference uses, not a copy of it, so both surfaces order results identically.
/// nucleo is only a candidate generator here: its path-scheme score ties a folder
/// with its own children, so final order has to be ours.
///
/// A superseded generation emits nothing. `LiveSearch::results` returns `None`
/// once a newer query has started, so a late redraw from a previous keystroke
/// cannot repaint the list — the "latest wins" rule lives there, once, rather
/// than being re-checked here.
fn emit_ranked_results(
    live: &Arc<lychi_core::file_search::live::LiveSearch>,
    generation: lychi_core::file_search::session::Generation,
    db: &Arc<redb::Database>,
    app: &AppHandle,
    search_id: u64,
) {
    use lychi_core::file_search::{RANK_POOL, rank};

    // Per group; folders and files are ranked independently so each fills its own
    // section without one starving the other.
    const PER_GROUP: usize = 25;

    let Some(results) = live.results(generation, RANK_POOL) else {
        tracing::debug!(
            ?generation,
            search_id,
            "[file-search] superseded — not emitting"
        );
        return; // superseded — a newer query owns the screen
    };
    tracing::debug!(
        query = %results.query,
        candidates = results.items.len(),
        complete = results.complete,
        search_id,
        "[file-search] emitting"
    );

    let frecency_scores = lychi_core::db::frecency::get_scores(db);
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let home = dirs::home_dir();

    let ranked = rank::rank(&results.query, results.items, |d| {
        frecency_recency_bonus(
            frecency_scores.get(&d.full_path).copied(),
            d.modified_secs,
            now_secs,
        )
    });
    let (folders, files) = rank::split_groups(ranked, PER_GROUP);

    // Build the emitted list: a "folders" section header, the folders, then a
    // "files" header, then the files. Headers use the `__separator__` sentinel
    // the CompletionsList already renders — no new UI. A section is omitted
    // entirely when empty, so an all-files query shows just the one group.
    //
    // Display score is a single descending integer so the frontend's own
    // score-sort preserves exactly this order (headers get a score above their
    // section's items but below the previous section's, keeping them in place).
    let mut out: Vec<FileSearchResult> = Vec::with_capacity(folders.len() + files.len() + 2);
    let mut next_score: u16 = (folders.len() + files.len() + 4) as u16;

    let mut push_group =
        |label: &str, group: Vec<rank::Ranked>, out: &mut Vec<FileSearchResult>| {
            if group.is_empty() {
                return;
            }
            out.push(section_header(label, &mut next_score));
            for r in group {
                out.push(finalize_row(
                    FileSearchResult {
                        label: rank::display_label(&r.data, home.as_deref()),
                        full_path: r.data.full_path.clone(),
                        is_dir: r.data.is_dir,
                        score: 0, // set by finalize_row from the display rank
                        description: r.description,
                        size_bytes: r.data.size_bytes,
                        modified_secs: r.data.modified_secs,
                    },
                    &mut next_score,
                ));
            }
        };
    push_group("folders", folders, &mut out);
    push_group("files", files, &mut out);

    let _ = app.emit(
        "lychi://file-search-results",
        FileSearchBatch {
            search_id,
            results: out,
            done: results.complete,
            has_ignore_rules: false,
        },
    );
}

/// Cancel any in-flight file search.
#[tauri::command]
#[specta::specta]
pub async fn cancel_file_search(state: State<'_, AppState>) -> Result<(), LychiError> {
    // Still used by the directory-listing path (`walk_and_emit`), which is a
    // plain walk with no session behind it.
    state.active_file_search.store(0, Ordering::SeqCst);
    // Drop the fuzzy session and bump the generation, so an in-flight redraw
    // that fires after the user dismissed cannot repaint the list.
    state.live_search.cancel();
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

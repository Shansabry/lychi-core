//! Tier 2 IDE workspace detection: read the editor's own config/session state.
//!
//! Covers GUI/menu-launched editors, where the process tree (Tier 1) only
//! shows `$HOME` as cwd and no folder arg. Fragile by nature (product- and
//! version-specific paths), so config dirs are derived dynamically from the WM
//! class and probed across native / Flatpak / Snap locations — no hardcoded
//! versions or absolute paths. Only runs when Tier 1 misses.

use std::path::{Path, PathBuf};

/// Detect the open project directory from the IDE's config state, keyed on the
/// window's WM class. `None` if unsupported or nothing found.
pub fn detect(wm_class: &str) -> Option<String> {
    let short = super::active_window::normalize_wm_class(wm_class);

    if let Some(product) = vscode_product_dir(&short) {
        return vscode_last_folder(product);
    }
    if short.starts_with("jetbrains-") {
        return jetbrains_last_project();
    }
    None
}

// ── VS Code family ──────────────────────────────────────────────────────

/// Map a normalized WM class to its VS Code-family config subdirectory name.
/// Forks reuse the exact `User/globalStorage/storage.json` layout under their
/// own product dir.
fn vscode_product_dir(short: &str) -> Option<&'static str> {
    match short {
        "code" => Some("Code"),
        "code-oss" => Some("Code - OSS"),
        "vscodium" | "codium" => Some("VSCodium"),
        "cursor" => Some("Cursor"),
        "windsurf" => Some("Windsurf"),
        _ => None,
    }
}

/// Read the last-active window's open folder from a VS Code-family
/// `globalStorage/storage.json`. Tries native, Flatpak, and Snap config roots.
fn vscode_last_folder(product: &str) -> Option<String> {
    for root in config_roots() {
        let storage = root
            .join(product)
            .join("User")
            .join("globalStorage")
            .join("storage.json");
        if let Some(path) = read_vscode_storage(&storage) {
            return Some(path);
        }
    }
    None
}

fn read_vscode_storage(storage: &Path) -> Option<String> {
    let text = std::fs::read_to_string(storage).ok()?;
    let json: serde_json::Value = serde_json::from_str(&text).ok()?;

    // Preferred: the focused window's folder.
    let ws = json.get("windowsState")?;
    let mut candidates: Vec<&str> = Vec::new();
    if let Some(f) = ws
        .get("lastActiveWindow")
        .and_then(|w| w.get("folder"))
        .and_then(|v| v.as_str())
    {
        candidates.push(f);
    }
    // Fallbacks: any currently-open folder window (hot-exit backups).
    if let Some(folders) = json
        .get("backupWorkspaces")
        .and_then(|b| b.get("folders"))
        .and_then(|v| v.as_array())
    {
        for entry in folders {
            if let Some(uri) = entry.get("folderUri").and_then(|v| v.as_str()) {
                candidates.push(uri);
            }
        }
    }

    for uri in candidates {
        if let Some(path) = super::ide_proc::uri_to_local_path(uri)
            && let Some(valid) = validate_dir(&path)
        {
            return Some(valid);
        }
    }
    None
}

// ── JetBrains family ────────────────────────────────────────────────────

/// The currently-open JetBrains project (opened="true", newest activation).
/// Globs the newest product config dir so versions aren't hardcoded.
fn jetbrains_last_project() -> Option<String> {
    let mut best: Option<(u64, String)> = None; // (activationTimestamp, path)

    for root in config_roots() {
        // JetBrains keeps per-product dirs (e.g. `IntelliJIdea2024.2`) directly
        // under a `JetBrains` root; Android Studio under `Google`.
        for brand in ["JetBrains", "Google"] {
            let Ok(products) = std::fs::read_dir(root.join(brand)) else {
                continue;
            };
            for product in products.flatten() {
                let xml = product.path().join("options").join("recentProjects.xml");
                if let Some((ts, path)) = parse_recent_projects(&xml)
                    && best.as_ref().is_none_or(|(b, _)| ts > *b)
                {
                    best = Some((ts, path));
                }
            }
        }
    }
    best.map(|(_, p)| p)
}

/// Parse the open project with the newest `activationTimestamp` from a
/// JetBrains `recentProjects.xml`. Lightweight string scan (no XML dep).
fn parse_recent_projects(xml: &Path) -> Option<(u64, String)> {
    let text = std::fs::read_to_string(xml).ok()?;
    let home = std::env::var("HOME").unwrap_or_default();

    let mut best: Option<(u64, String)> = None;
    // Each project is `<entry key="$PROJECT_DIR$/…">` followed by a
    // `RecentProjectMetaInfo` with `opened` / `activationTimestamp` fields.
    for chunk in text.split("<entry key=\"").skip(1) {
        let Some(raw_key) = chunk.split('"').next() else {
            continue;
        };
        // Only currently-open projects.
        if !chunk.contains("opened=\"true\"") {
            continue;
        }
        let ts = extract_attr(chunk, "activationTimestamp")
            .or_else(|| extract_attr(chunk, "projectOpenTimestamp"))
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
        let path = raw_key.replace("$USER_HOME$", &home);
        if best.as_ref().is_none_or(|(b, _)| ts > *b) {
            best = Some((ts, path));
        }
    }
    let (ts, path) = best?;
    validate_dir(Path::new(&path)).map(|p| (ts, p))
}

/// Extract `name="attr" value="X"` → `X` from a JetBrains option chunk.
fn extract_attr<'a>(chunk: &'a str, attr: &str) -> Option<&'a str> {
    let needle = format!("name=\"{attr}\" value=\"");
    let start = chunk.find(&needle)? + needle.len();
    chunk[start..].split('"').next()
}

// ── Shared ──────────────────────────────────────────────────────────────

/// Candidate config roots: native `~/.config`, plus Flatpak and Snap
/// relocations. Editors installed via Flatpak/Snap store config elsewhere.
fn config_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        if !xdg.is_empty() {
            roots.push(PathBuf::from(xdg));
        }
    }
    let home = std::env::var("HOME").unwrap_or_default();
    if !home.is_empty() {
        let home = PathBuf::from(&home);
        roots.push(home.join(".config"));
        // Flatpak: ~/.var/app/<app-id>/config
        push_glob_children(&mut roots, &home.join(".var/app"), "config");
        // Snap: ~/snap/<app>/current/.config
        push_glob_children(&mut roots, &home.join("snap"), "current/.config");
    }
    roots
}

/// For each child dir of `base`, push `child/<suffix>` if it exists.
fn push_glob_children(out: &mut Vec<PathBuf>, base: &Path, suffix: &str) {
    if let Ok(entries) = std::fs::read_dir(base) {
        for e in entries.flatten() {
            let p = e.path().join(suffix);
            if p.is_dir() {
                out.push(p);
            }
        }
    }
}

/// A resolved config path must be a real, existing project directory. Reuses
/// the shared marker check so only genuine workspaces are returned.
fn validate_dir(path: &Path) -> Option<String> {
    if !path.is_dir() {
        return None;
    }
    super::ide::which_project_marker(path)?;
    Some(path.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vscode_product_mapping() {
        assert_eq!(vscode_product_dir("code"), Some("Code"));
        assert_eq!(vscode_product_dir("cursor"), Some("Cursor"));
        assert_eq!(vscode_product_dir("codium"), Some("VSCodium"));
        assert_eq!(vscode_product_dir("konsole"), None); // not an editor
    }

    #[test]
    fn read_storage_extracts_last_active_folder() {
        // Point HOME at a temp dir with a real project + a storage.json.
        let tmp = std::env::temp_dir().join(format!("lychi-ide-cfg-{}", std::process::id()));
        let proj = tmp.join("myproj");
        std::fs::create_dir_all(proj.join(".git")).unwrap();
        let uri = format!("file://{}", proj.display());
        let storage = tmp.join("storage.json");
        std::fs::write(
            &storage,
            format!(r#"{{"windowsState":{{"lastActiveWindow":{{"folder":"{uri}"}}}}}}"#),
        )
        .unwrap();

        let got = read_vscode_storage(&storage);
        assert_eq!(got, Some(proj.to_string_lossy().into_owned()));

        // A remote URI must NOT resolve to a local path.
        std::fs::write(
            &storage,
            r#"{"windowsState":{"lastActiveWindow":{"folder":"vscode-remote://ssh-remote+h/root/x"}}}"#,
        )
        .unwrap();
        assert_eq!(read_vscode_storage(&storage), None);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn jetbrains_parses_opened_project() {
        let tmp = std::env::temp_dir().join(format!("lychi-jb-{}", std::process::id()));
        let proj = tmp.join("api");
        std::fs::create_dir_all(proj.join(".git")).unwrap();
        let xml = format!(
            r#"<application><component name="RecentProjectsManager">
              <option name="additionalInfo"><map>
                <entry key="{}">
                  <value><RecentProjectMetaInfo opened="true">
                    <option name="activationTimestamp" value="1700000000000" />
                  </RecentProjectMetaInfo></value>
                </entry>
                <entry key="/old/closed">
                  <value><RecentProjectMetaInfo opened="false">
                    <option name="activationTimestamp" value="1600000000000" />
                  </RecentProjectMetaInfo></value>
                </entry>
              </map></option></component></application>"#,
            proj.display()
        );
        let xml_path = tmp.join("recentProjects.xml");
        std::fs::write(&xml_path, xml).unwrap();

        let got = parse_recent_projects(&xml_path);
        assert_eq!(
            got.map(|(_, p)| p),
            Some(proj.to_string_lossy().into_owned())
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }
}

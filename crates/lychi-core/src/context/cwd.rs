//! Terminal/IDE CWD detection via `/proc`.
//!
//! For terminals: finds the shell child process and reads its CWD.
//! For IDEs (VS Code, JetBrains, etc.): scans descendant processes for
//! workspace CWDs, since the main process CWD is typically `$HOME`.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

/// Detect the working directory of a terminal or IDE process.
pub fn detect(pid: u32, wm_class: &str, title: &str) -> Option<String> {
    let home = std::env::var("HOME").unwrap_or_default();

    if is_ide(wm_class) {
        return detect_workspace_from_descendants(pid, &home, title);
    }

    // For terminals: walk to the shell child and read its CWD
    let shell_pid = find_foreground_child(pid)?;
    let cwd = fs::read_link(format!("/proc/{shell_pid}/cwd")).ok()?;
    let cwd_str = cwd.to_string_lossy();

    if cwd_str == home || !cwd.is_dir() {
        return None;
    }

    Some(cwd_str.into_owned())
}

/// Known IDE WM classes whose main process CWD is unreliable.
fn is_ide(wm_class: &str) -> bool {
    const IDES: &[&str] = &[
        "code",
        "codium",
        "vscodium",
        "cursor",
        "windsurf",
        "zed",
        "jetbrains-idea",
        "jetbrains-clion",
        "jetbrains-pycharm",
        "jetbrains-webstorm",
        "jetbrains-goland",
        "jetbrains-rider",
        "jetbrains-rustrover",
        "jetbrains-fleet",
    ];
    IDES.iter().any(|ide| wm_class.contains(ide))
}

/// Scan descendant processes of `pid`, collect their CWDs, and return the
/// most likely workspace directory.
///
/// Only scans processes that share the same executable as the IDE (avoids
/// reading `/proc/*/status` for every process on the system). Falls back
/// to full `/proc` scan if the exe can't be resolved.
fn detect_workspace_from_descendants(pid: u32, home: &str, title: &str) -> Option<String> {
    let ide_exe = fs::read_link(format!("/proc/{pid}/exe")).ok();
    let descendants = collect_descendants(pid, ide_exe.as_deref());

    let mut counts: HashMap<String, u32> = HashMap::new();

    for child_pid in &descendants {
        let Ok(cwd) = fs::read_link(format!("/proc/{child_pid}/cwd")) else {
            continue;
        };
        let cwd_str = cwd.to_string_lossy();

        if is_junk_path(&cwd_str, home) || !cwd.is_dir() {
            continue;
        }

        *counts.entry(cwd_str.into_owned()).or_insert(0) += 1;
    }

    if counts.is_empty() {
        return None;
    }

    if counts.len() == 1 {
        return counts.into_keys().next();
    }

    // Multiple workspaces — use window title to pick the focused one.
    // VS Code title: "file.rs - ProjectName - Visual Studio Code"
    if let Some(project) = project_name_from_title(title) {
        let project_lower = project.to_lowercase();
        for path in counts.keys() {
            let basename = std::path::Path::new(path)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_lowercase();
            if basename == project_lower {
                return Some(path.clone());
            }
        }
    }

    // Fallback: most common CWD
    counts
        .into_iter()
        .max_by_key(|(_, count)| *count)
        .map(|(path, _)| path)
}

/// Paths that are never useful as workspace context.
fn is_junk_path(path: &str, home: &str) -> bool {
    path == home
        || path == "/"
        || path.starts_with("/proc")
        || path.contains("/.vscode/extensions/")
        || path.contains("/.cursor/extensions/")
        || path.contains("/.local/share/")
        || path.contains("/telemetry/")
}

/// Extract project name from IDE window title.
/// "file.rs - ProjectName - Visual Studio Code" → "ProjectName"
/// "ProjectName - Visual Studio Code" → "ProjectName"
fn project_name_from_title(title: &str) -> Option<&str> {
    let parts: Vec<&str> = title.split(" - ").collect();
    if parts.len() >= 3 {
        let project = parts[parts.len() - 2].trim();
        if !project.is_empty() {
            return Some(project);
        }
    }
    if parts.len() == 2 {
        let project = parts[0].trim().trim_start_matches('●').trim();
        if !project.is_empty() {
            return Some(project);
        }
    }
    None
}

/// Collect descendant PIDs of `root_pid`.
///
/// Optimized path: if `ide_exe` is known, only scan `/proc` entries whose
/// `/proc/<pid>/exe` matches the same binary. This narrows ~500 entries
/// down to ~20-50 for a typical IDE. Falls back to full scan if exe is None.
fn collect_descendants(root_pid: u32, ide_exe: Option<&std::path::Path>) -> Vec<u32> {
    let mut parent_map: HashMap<u32, Vec<u32>> = HashMap::new();

    let Ok(entries) = fs::read_dir("/proc") else {
        return Vec::new();
    };

    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };

        // Fast filter: skip processes that don't share the IDE's executable
        if let Some(exe) = ide_exe {
            if let Ok(proc_exe) = fs::read_link(format!("/proc/{pid}/exe")) {
                if proc_exe != exe {
                    continue;
                }
            } else {
                continue;
            }
        }

        // Read PPid from /proc/<pid>/status
        if let Ok(status) = fs::read_to_string(format!("/proc/{pid}/status")) {
            for line in status.lines() {
                if let Some(ppid_str) = line.strip_prefix("PPid:\t") {
                    if let Ok(ppid) = ppid_str.trim().parse::<u32>() {
                        parent_map.entry(ppid).or_default().push(pid);
                    }
                    break;
                }
            }
        }
    }

    // BFS from root_pid
    let mut result = Vec::new();
    let mut queue = vec![root_pid];
    while let Some(pid) = queue.pop() {
        if let Some(children) = parent_map.get(&pid) {
            for &child in children {
                result.push(child);
                queue.push(child);
            }
        }
    }
    result
}

/// Find the foreground child process of a terminal.
///
/// Walk the process tree: terminal -> shell -> possibly a running command.
/// Returns the deepest child with a readable CWD.
fn find_foreground_child(pid: u32) -> Option<u32> {
    let children = get_children(pid);

    if children.is_empty() {
        return Some(pid);
    }

    for &child in &children {
        if fs::read_link(format!("/proc/{child}/cwd")).is_ok() {
            if let Some(deep_pid) = find_foreground_child(child)
                && fs::read_link(format!("/proc/{deep_pid}/cwd")).is_ok()
            {
                return Some(deep_pid);
            }
            return Some(child);
        }
    }

    Some(pid)
}

/// Get direct child PIDs of a process via `/proc/<pid>/task/<pid>/children`.
fn get_children(pid: u32) -> Vec<u32> {
    let path = PathBuf::from(format!("/proc/{pid}/task/{pid}/children"));
    fs::read_to_string(&path)
        .unwrap_or_default()
        .split_whitespace()
        .filter_map(|s| s.parse::<u32>().ok())
        .collect()
}

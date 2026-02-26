//! Project type detection from marker files.
//!
//! Walks upward from a directory checking for project markers like
//! `Cargo.toml`, `package.json`, `pyproject.toml`, etc.
//! Also discovers project scripts/targets for contextual suggestions.

use std::path::Path;

use super::{ProjectContext, ProjectKind, ProjectScript};

/// Marker files and their corresponding project kind, in priority order.
const MARKERS: &[(&str, ProjectKind)] = &[
    ("Cargo.toml", ProjectKind::Rust),
    ("package.json", ProjectKind::Node),
    ("pubspec.yaml", ProjectKind::Flutter),
    ("pyproject.toml", ProjectKind::Python),
    ("setup.py", ProjectKind::Python),
    ("requirements.txt", ProjectKind::Python),
    ("go.mod", ProjectKind::Go),
    ("Dockerfile", ProjectKind::Docker),
];

/// Detect project type by walking upward from `dir`.
pub fn detect(dir: &str) -> Option<ProjectContext> {
    let mut current = Path::new(dir);

    for _ in 0..50 {
        for (marker, kind) in MARKERS {
            if current.join(marker).exists() {
                let has_compose = current.join("docker-compose.yml").exists()
                    || current.join("compose.yml").exists();
                let package_manager = if *kind == ProjectKind::Node {
                    Some(detect_package_manager(current))
                } else {
                    None
                };
                let scripts = discover_scripts(current, kind, package_manager.as_deref());
                return Some(ProjectContext {
                    root: current.to_string_lossy().into_owned(),
                    kind: kind.clone(),
                    has_compose,
                    scripts,
                    package_manager,
                });
            }
        }
        current = current.parent()?;
    }

    None
}

/// Detect the Node.js package manager from lockfile presence.
///
/// Priority: bun > pnpm > yarn > npm (more specific lockfiles first).
fn detect_package_manager(root: &Path) -> String {
    if root.join("bun.lockb").exists() || root.join("bun.lock").exists() {
        "bun".to_string()
    } else if root.join("pnpm-lock.yaml").exists() {
        "pnpm".to_string()
    } else if root.join("yarn.lock").exists() {
        "yarn".to_string()
    } else {
        "npm".to_string()
    }
}

/// Discover project scripts/targets based on project kind.
fn discover_scripts(
    root: &Path,
    kind: &ProjectKind,
    pkg_manager: Option<&str>,
) -> Vec<ProjectScript> {
    let mut scripts = Vec::new();

    match kind {
        ProjectKind::Node => scripts.extend(parse_node_scripts(root, pkg_manager.unwrap_or("npm"))),
        ProjectKind::Rust => scripts.extend(parse_cargo_scripts(root)),
        ProjectKind::Python => scripts.extend(parse_python_scripts(root)),
        ProjectKind::Go => scripts.extend(parse_go_scripts(root)),
        ProjectKind::Flutter => scripts.extend(parse_flutter_scripts(root)),
        _ => {}
    }

    // Makefile, Justfile, and Taskfile are common across all project types
    scripts.extend(parse_makefile_targets(root));
    scripts.extend(parse_justfile_recipes(root));
    scripts.extend(parse_taskfile_tasks(root));

    scripts
}

/// Parse script names from `package.json` using the detected package manager.
fn parse_node_scripts(root: &Path, pkg_manager: &str) -> Vec<ProjectScript> {
    let Ok(content) = std::fs::read_to_string(root.join("package.json")) else {
        return Vec::new();
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) else {
        return Vec::new();
    };
    // bun uses "bun run", pnpm uses "pnpm run", yarn uses "yarn", npm uses "npm run"
    let runner = match pkg_manager {
        "yarn" => "yarn".to_string(),
        other => format!("{other} run"),
    };
    json.get("scripts")
        .and_then(|s| s.as_object())
        .map(|obj| {
            obj.keys()
                .map(|k| ProjectScript {
                    runner: runner.clone(),
                    name: k.clone(),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Parse target names from a `Makefile`.
fn parse_makefile_targets(root: &Path) -> Vec<ProjectScript> {
    let Ok(content) = std::fs::read_to_string(root.join("Makefile")) else {
        return Vec::new();
    };
    content
        .lines()
        .filter_map(|line| {
            // Target lines start at column 0, end with ':'
            if line.starts_with('\t') || line.starts_with('#') || line.starts_with(' ') {
                return None;
            }
            let name = line.split(':').next()?.trim();
            if name.is_empty() || name.contains('=') || name.starts_with('.') {
                return None;
            }
            Some(ProjectScript {
                runner: "make".to_string(),
                name: name.to_string(),
            })
        })
        .collect()
}

/// Parse recipe names from a `Justfile` or `justfile`.
fn parse_justfile_recipes(root: &Path) -> Vec<ProjectScript> {
    let content = std::fs::read_to_string(root.join("Justfile"))
        .or_else(|_| std::fs::read_to_string(root.join("justfile")));
    let Ok(content) = content else {
        return Vec::new();
    };
    content
        .lines()
        .filter_map(|line| {
            if line.starts_with(' ')
                || line.starts_with('\t')
                || line.starts_with('#')
                || line.is_empty()
            {
                return None;
            }
            // Recipe lines: `name:` or `name arg:` — take the first word
            let first_token = line.split_whitespace().next()?;
            let name = first_token.trim_end_matches(':');
            if name.is_empty() || name.starts_with('@') || name.contains('=') {
                return None;
            }
            // Skip lines that are just variable assignments or settings
            if line.contains(":=") || line.starts_with("set ") || line.starts_with("export ") {
                return None;
            }
            Some(ProjectScript {
                runner: "just".to_string(),
                name: name.to_string(),
            })
        })
        .collect()
}

/// Parse Cargo workspace members and binary targets from `Cargo.toml`.
fn parse_cargo_scripts(root: &Path) -> Vec<ProjectScript> {
    if !root.join("Cargo.toml").exists() {
        return Vec::new();
    }

    let mut scripts = vec![
        ProjectScript {
            runner: "cargo".to_string(),
            name: "build".to_string(),
        },
        ProjectScript {
            runner: "cargo".to_string(),
            name: "test".to_string(),
        },
        ProjectScript {
            runner: "cargo".to_string(),
            name: "run".to_string(),
        },
        ProjectScript {
            runner: "cargo".to_string(),
            name: "check".to_string(),
        },
        ProjectScript {
            runner: "cargo".to_string(),
            name: "clippy".to_string(),
        },
        ProjectScript {
            runner: "cargo".to_string(),
            name: "fmt".to_string(),
        },
    ];

    // Detect cargo-watch if Cargo.toml has [dependencies] or if it's commonly used
    if root.join("rust-toolchain.toml").exists() || root.join("rust-toolchain").exists() {
        scripts.push(ProjectScript {
            runner: "cargo".to_string(),
            name: "bench".to_string(),
        });
    }

    scripts
}

/// Parse scripts from `pyproject.toml` (PEP 621 + Poetry).
fn parse_python_scripts(root: &Path) -> Vec<ProjectScript> {
    let mut scripts = Vec::new();

    // Try pyproject.toml first
    if let Ok(content) = std::fs::read_to_string(root.join("pyproject.toml")) {
        // Detect [project.scripts] section (PEP 621)
        let mut in_scripts = false;
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed == "[project.scripts]" || trimmed == "[tool.poetry.scripts]" {
                in_scripts = true;
                continue;
            }
            if trimmed.starts_with('[') {
                in_scripts = false;
                continue;
            }
            if in_scripts && let Some((key, _)) = trimmed.split_once('=') {
                let name = key.trim().trim_matches('"');
                if !name.is_empty() {
                    scripts.push(ProjectScript {
                        runner: "python -m".to_string(),
                        name: name.to_string(),
                    });
                }
            }
        }

        // Detect package manager: poetry, pdm, hatch, uv, pip
        if content.contains("[tool.poetry]") {
            scripts.push(ProjectScript {
                runner: "poetry".to_string(),
                name: "install".to_string(),
            });
            scripts.push(ProjectScript {
                runner: "poetry".to_string(),
                name: "run".to_string(),
            });
            scripts.push(ProjectScript {
                runner: "poetry".to_string(),
                name: "shell".to_string(),
            });
        } else if content.contains("[tool.pdm]") {
            scripts.push(ProjectScript {
                runner: "pdm".to_string(),
                name: "install".to_string(),
            });
            scripts.push(ProjectScript {
                runner: "pdm".to_string(),
                name: "run".to_string(),
            });
        } else if content.contains("[tool.hatch]") {
            scripts.push(ProjectScript {
                runner: "hatch".to_string(),
                name: "run".to_string(),
            });
            scripts.push(ProjectScript {
                runner: "hatch".to_string(),
                name: "build".to_string(),
            });
        }
    }

    // Common Python project commands
    if root.join("manage.py").exists() {
        // Django project
        scripts.push(ProjectScript {
            runner: "python manage.py".to_string(),
            name: "runserver".to_string(),
        });
        scripts.push(ProjectScript {
            runner: "python manage.py".to_string(),
            name: "migrate".to_string(),
        });
        scripts.push(ProjectScript {
            runner: "python manage.py".to_string(),
            name: "test".to_string(),
        });
    }

    // If uv.lock exists, it's a uv project
    if root.join("uv.lock").exists() {
        scripts.push(ProjectScript {
            runner: "uv".to_string(),
            name: "sync".to_string(),
        });
        scripts.push(ProjectScript {
            runner: "uv run".to_string(),
            name: "python".to_string(),
        });
    }

    // pytest if test directory or conftest.py exists
    if root.join("tests").exists() || root.join("conftest.py").exists() {
        scripts.push(ProjectScript {
            runner: "pytest".to_string(),
            name: "".to_string(),
        });
    }

    scripts
}

/// Parse common Go project scripts/tasks.
fn parse_go_scripts(root: &Path) -> Vec<ProjectScript> {
    let mut scripts = vec![
        ProjectScript {
            runner: "go".to_string(),
            name: "build".to_string(),
        },
        ProjectScript {
            runner: "go".to_string(),
            name: "test ./...".to_string(),
        },
        ProjectScript {
            runner: "go".to_string(),
            name: "run .".to_string(),
        },
        ProjectScript {
            runner: "go".to_string(),
            name: "mod tidy".to_string(),
        },
        ProjectScript {
            runner: "go".to_string(),
            name: "vet ./...".to_string(),
        },
    ];

    // Detect air (hot-reload) config
    if root.join(".air.toml").exists() || root.join("air.toml").exists() {
        scripts.push(ProjectScript {
            runner: "air".to_string(),
            name: "".to_string(),
        });
    }

    scripts
}

/// Parse Flutter/Dart project scripts from `pubspec.yaml`.
fn parse_flutter_scripts(root: &Path) -> Vec<ProjectScript> {
    // This is only called when pubspec.yaml exists (ProjectKind::Flutter detected).
    // Check if it's a Flutter project (has flutter dependency) or pure Dart.
    let is_flutter_project = std::fs::read_to_string(root.join("pubspec.yaml"))
        .map(|c| c.contains("flutter:") || c.contains("flutter_test:"))
        .unwrap_or(false);

    if is_flutter_project {
        vec![
            ProjectScript {
                runner: "flutter".to_string(),
                name: "run".to_string(),
            },
            ProjectScript {
                runner: "flutter".to_string(),
                name: "build apk".to_string(),
            },
            ProjectScript {
                runner: "flutter".to_string(),
                name: "build web".to_string(),
            },
            ProjectScript {
                runner: "flutter".to_string(),
                name: "test".to_string(),
            },
            ProjectScript {
                runner: "flutter".to_string(),
                name: "pub get".to_string(),
            },
            ProjectScript {
                runner: "flutter".to_string(),
                name: "analyze".to_string(),
            },
            ProjectScript {
                runner: "flutter".to_string(),
                name: "clean".to_string(),
            },
        ]
    } else {
        vec![
            ProjectScript {
                runner: "dart".to_string(),
                name: "run".to_string(),
            },
            ProjectScript {
                runner: "dart".to_string(),
                name: "test".to_string(),
            },
            ProjectScript {
                runner: "dart".to_string(),
                name: "pub get".to_string(),
            },
            ProjectScript {
                runner: "dart".to_string(),
                name: "analyze".to_string(),
            },
            ProjectScript {
                runner: "dart".to_string(),
                name: "compile exe".to_string(),
            },
        ]
    }
}

/// Parse task names from a `Taskfile.yml` (https://taskfile.dev).
fn parse_taskfile_tasks(root: &Path) -> Vec<ProjectScript> {
    let content = std::fs::read_to_string(root.join("Taskfile.yml")).or_else(|_| {
        std::fs::read_to_string(root.join("Taskfile.yaml"))
            .or_else(|_| std::fs::read_to_string(root.join("taskfile.yml")))
    });
    let Ok(content) = content else {
        return Vec::new();
    };
    // Simple YAML parsing: task names are top-level keys under `tasks:` section
    let mut in_tasks = false;
    content
        .lines()
        .filter_map(|line| {
            if line.trim() == "tasks:" {
                in_tasks = true;
                return None;
            }
            if !in_tasks {
                return None;
            }
            // Task entries are indented with 2 spaces and end with ':'
            if line.starts_with("  ") && !line.starts_with("    ") {
                let name = line.trim().trim_end_matches(':');
                if !name.is_empty() && !name.starts_with('#') {
                    return Some(ProjectScript {
                        runner: "task".to_string(),
                        name: name.to_string(),
                    });
                }
            }
            // A new top-level key ends the tasks section
            if !line.starts_with(' ') && !line.is_empty() && !line.starts_with('#') {
                in_tasks = false;
            }
            None
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn test_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("lychi_test_{name}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn test_parse_node_scripts() {
        let dir = test_dir("node_scripts");
        fs::write(
            dir.join("package.json"),
            r#"{"scripts": {"dev": "vite", "build": "tsc && vite build", "test": "vitest"}}"#,
        )
        .unwrap();
        let scripts = parse_node_scripts(&dir, "npm");
        let names: Vec<&str> = scripts.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"dev"));
        assert!(names.contains(&"build"));
        assert!(names.contains(&"test"));
        assert!(scripts.iter().all(|s| s.runner == "npm run"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_parse_node_scripts_pnpm() {
        let dir = test_dir("node_scripts_pnpm");
        fs::write(
            dir.join("package.json"),
            r#"{"scripts": {"dev": "vite", "build": "tsc"}}"#,
        )
        .unwrap();
        let scripts = parse_node_scripts(&dir, "pnpm");
        assert!(scripts.iter().all(|s| s.runner == "pnpm run"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_parse_node_scripts_yarn() {
        let dir = test_dir("node_scripts_yarn");
        fs::write(dir.join("package.json"), r#"{"scripts": {"dev": "vite"}}"#).unwrap();
        let scripts = parse_node_scripts(&dir, "yarn");
        assert!(scripts.iter().all(|s| s.runner == "yarn"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_detect_package_manager() {
        let dir = test_dir("pkg_manager");
        fs::write(dir.join("package.json"), r#"{"name": "test"}"#).unwrap();

        // Default: npm
        assert_eq!(detect_package_manager(&dir), "npm");

        // pnpm
        fs::write(dir.join("pnpm-lock.yaml"), "").unwrap();
        assert_eq!(detect_package_manager(&dir), "pnpm");
        fs::remove_file(dir.join("pnpm-lock.yaml")).unwrap();

        // yarn
        fs::write(dir.join("yarn.lock"), "").unwrap();
        assert_eq!(detect_package_manager(&dir), "yarn");
        fs::remove_file(dir.join("yarn.lock")).unwrap();

        // bun
        fs::write(dir.join("bun.lockb"), "").unwrap();
        assert_eq!(detect_package_manager(&dir), "bun");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_parse_makefile_targets() {
        let dir = test_dir("makefile_targets");
        fs::write(
            dir.join("Makefile"),
            "build:\n\tcargo build\n\ntest:\n\tcargo test\n\n.PHONY: build test\n",
        )
        .unwrap();
        let targets = parse_makefile_targets(&dir);
        let names: Vec<&str> = targets.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"build"));
        assert!(names.contains(&"test"));
        assert!(!names.contains(&".PHONY"));
        assert!(targets.iter().all(|s| s.runner == "make"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_parse_justfile_recipes() {
        let dir = test_dir("justfile_recipes");
        fs::write(
            dir.join("justfile"),
            "# A justfile\n\nbuild:\n  cargo build\n\ntest:\n  cargo test\n\nexport FOO := \"bar\"\n",
        )
        .unwrap();
        let recipes = parse_justfile_recipes(&dir);
        let names: Vec<&str> = recipes.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"build"));
        assert!(names.contains(&"test"));
        assert!(recipes.iter().all(|s| s.runner == "just"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_depth_detection() {
        let dir = test_dir("depth_detect");
        let nested = dir.join("src").join("lib");
        fs::create_dir_all(&nested).unwrap();
        fs::write(dir.join("Cargo.toml"), "[package]\nname = \"test\"").unwrap();

        let ctx = detect(nested.to_str().unwrap());
        assert!(ctx.is_some());
        let ctx = ctx.unwrap();
        assert_eq!(ctx.kind, ProjectKind::Rust);
        assert_eq!(ctx.root, dir.to_string_lossy().as_ref());
        let _ = fs::remove_dir_all(&dir);
    }
}

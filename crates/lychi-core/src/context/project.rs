//! Project type detection from marker files.
//!
//! Walks upward from a directory checking for project markers like
//! `Cargo.toml`, `package.json`, `pyproject.toml`, etc.

use std::path::Path;

use super::{ProjectContext, ProjectKind};

/// Marker files and their corresponding project kind, in priority order.
const MARKERS: &[(&str, ProjectKind)] = &[
    ("Cargo.toml", ProjectKind::Rust),
    ("package.json", ProjectKind::Node),
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
                return Some(ProjectContext {
                    root: current.to_string_lossy().into_owned(),
                    kind: kind.clone(),
                });
            }
        }
        current = current.parent()?;
    }

    None
}

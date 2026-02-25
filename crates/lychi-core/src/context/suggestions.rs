//! Contextual completion suggestions based on detected environment.
//!
//! Maps `EnvironmentContext` → `Vec<CompletionItem>` to show relevant
//! commands when the user opens Lychi with an empty input.

use crate::action_registry::CompletionItem;

use super::{EnvironmentContext, ProjectKind};

/// Generate contextual completions based on current environment.
///
/// Returns up to 5 suggestions based on detected context.
pub fn suggest(ctx: &EnvironmentContext) -> Vec<CompletionItem> {
    let mut items = Vec::new();

    // Git context suggestions
    if let Some(ref git) = ctx.git {
        if git.dirty {
            items.push(completion("git commit", "Commit staged changes", 100));
            items.push(completion("git diff", "View uncommitted changes", 95));
        } else {
            items.push(completion("git pull", "Pull latest changes", 100));
            items.push(completion("git push", "Push commits to remote", 95));
        }
        items.push(completion("git stash", "Stash current changes", 85));
    }

    // Project-specific suggestions
    if let Some(ref project) = ctx.project {
        match project.kind {
            ProjectKind::Rust => {
                items.push(completion("run cargo build", "Build the project", 90));
                items.push(completion("run cargo test", "Run tests", 88));
                items.push(completion("run cargo run", "Run the project", 86));
            }
            ProjectKind::Node => {
                items.push(completion("run npm run dev", "Start dev server", 90));
                items.push(completion("run npm test", "Run tests", 88));
                items.push(completion("run npm install", "Install dependencies", 86));
            }
            ProjectKind::Python => {
                items.push(completion("run python main.py", "Run main script", 90));
                items.push(completion("run pytest", "Run tests", 88));
                items.push(completion(
                    "run pip install -r requirements.txt",
                    "Install deps",
                    86,
                ));
            }
            ProjectKind::Go => {
                items.push(completion("run go build", "Build the project", 90));
                items.push(completion("run go test ./...", "Run all tests", 88));
                items.push(completion("run go run .", "Run the project", 86));
            }
            _ => {}
        }
    }

    // Docker container suggestions
    if let Some(ref docker) = ctx.docker
        && !docker.containers.is_empty()
    {
        items.push(completion("run docker ps", "List running containers", 80));
        // Suggest logs for first container
        if let Some(first) = docker.containers.first() {
            items.push(completion(
                &format!("run docker logs {}", first.name),
                &format!("Logs for {}", first.name),
                78,
            ));
        }
    }

    // Truncate to 5 most relevant
    items.truncate(5);
    items
}

fn completion(label: &str, description: &str, score: u16) -> CompletionItem {
    CompletionItem {
        label: label.to_string(),
        icon_path: Some("__context__".to_string()),
        score,
        description: Some(description.to_string()),
    }
}

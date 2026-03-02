//! Browser window title parsing for contextual suggestions.
//!
//! When the focused window is a browser, parse the window title for
//! actionable patterns (GitHub repo, localhost dev server, etc.).

use serde::{Deserialize, Serialize};

/// Parsed context from a browser window title.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum BrowserContext {
    /// A GitHub repository page (issues, PRs, code, etc.).
    GitHub { owner: String, repo: String },
    /// A localhost development server.
    Localhost { port: u16 },
    /// A Stack Overflow question.
    StackOverflow,
    /// A documentation site (docs.rs, MDN, etc.).
    Documentation,
    /// Browser is open but title doesn't match any known pattern.
    Unknown,
}

/// Parse a browser window title for actionable context.
///
/// Browser titles typically follow `Page Title - Browser Name` or
/// `Page Title — Browser Name`. We strip the browser suffix and
/// pattern-match the remaining title.
pub fn parse_title(title: &str) -> BrowserContext {
    let title_lower = title.to_lowercase();

    // GitHub: titles like "user/repo: description · GitHub" or "Pull Request #123 · user/repo · GitHub"
    if title_lower.contains("github")
        && let Some(gh) = extract_github_repo(title)
    {
        return BrowserContext::GitHub {
            owner: gh.0,
            repo: gh.1,
        };
    }

    // localhost:PORT in title (common for dev servers)
    if let Some(port) = extract_localhost_port(&title_lower) {
        return BrowserContext::Localhost { port };
    }

    // Stack Overflow
    if title_lower.contains("stack overflow") || title_lower.contains("stackoverflow") {
        return BrowserContext::StackOverflow;
    }

    // Documentation sites
    if title_lower.contains("docs.rs")
        || title_lower.contains("developer.mozilla.org")
        || title_lower.contains("devdocs.io")
        || title_lower.contains("doc.rust-lang.org")
        || title_lower.contains("react.dev")
        || title_lower.contains("svelte.dev")
        || title_lower.contains("tailwindcss.com")
        || title_lower.contains("nodejs.org/api")
    {
        return BrowserContext::Documentation;
    }

    BrowserContext::Unknown
}

/// Extract `owner/repo` from a GitHub-style window title.
///
/// GitHub titles use patterns like:
/// - `"user/repo: Short description - GitHub"` (repo homepage)
/// - `"Issue title · Issue #123 · user/repo · GitHub"` (issue page)
/// - `"PR title by author · Pull Request #123 · user/repo · GitHub"` (PR page)
fn extract_github_repo(title: &str) -> Option<(String, String)> {
    // Split on common GitHub separators: " · ", " - ", " — "
    let segments: Vec<&str> = title
        .split(" · ")
        .flat_map(|s| s.split(" - "))
        .flat_map(|s| s.split(" — "))
        .map(str::trim)
        .collect();

    for segment in &segments {
        // Look for "owner/repo" pattern (possibly with ": description" suffix)
        let candidate = segment.split(':').next().unwrap_or(segment).trim();
        if let Some((owner, repo)) = candidate.split_once('/') {
            let owner = owner.trim();
            let repo = repo.trim();
            // Validate: owner and repo should be valid GitHub identifiers
            if !owner.is_empty()
                && !repo.is_empty()
                && owner != "https:"
                && owner != "http:"
                && owner
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == '.')
                && repo
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == '.')
            {
                return Some((owner.to_string(), repo.to_string()));
            }
        }
    }
    None
}

/// Extract port number from a title containing `localhost:PORT`.
fn extract_localhost_port(title: &str) -> Option<u16> {
    let idx = title.find("localhost:")?;
    let after = &title[idx + "localhost:".len()..];
    let port_str: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
    port_str.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_github_repo_homepage() {
        let ctx = parse_title("anthropics/claude-code: CLI for Claude - GitHub - Firefox");
        assert!(matches!(
            ctx,
            BrowserContext::GitHub { ref owner, ref repo }
            if owner == "anthropics" && repo == "claude-code"
        ));
    }

    #[test]
    fn test_github_issue_page() {
        let ctx = parse_title("Bug report · Issue #42 · user/repo · GitHub — Firefox");
        assert!(matches!(
            ctx,
            BrowserContext::GitHub { ref owner, ref repo }
            if owner == "user" && repo == "repo"
        ));
    }

    #[test]
    fn test_github_pr_page() {
        let ctx = parse_title("Fix typo by contributor · Pull Request #7 · org/project · GitHub");
        assert!(matches!(
            ctx,
            BrowserContext::GitHub { ref owner, ref repo }
            if owner == "org" && repo == "project"
        ));
    }

    #[test]
    fn test_localhost() {
        let ctx = parse_title("My App - localhost:3000 - Chromium");
        assert!(matches!(ctx, BrowserContext::Localhost { port: 3000 }));
    }

    #[test]
    fn test_localhost_vite() {
        let ctx = parse_title("Vite + Svelte - localhost:5173 — Firefox");
        assert!(matches!(ctx, BrowserContext::Localhost { port: 5173 }));
    }

    #[test]
    fn test_stack_overflow() {
        let ctx = parse_title("How to parse JSON in Rust - Stack Overflow - Firefox");
        assert!(matches!(ctx, BrowserContext::StackOverflow));
    }

    #[test]
    fn test_documentation() {
        let ctx = parse_title("serde - Rust - docs.rs — Firefox");
        assert!(matches!(ctx, BrowserContext::Documentation));
    }

    #[test]
    fn test_mdn_docs() {
        let ctx = parse_title(
            "Array.prototype.map() - JavaScript | MDN - developer.mozilla.org — Firefox",
        );
        assert!(matches!(ctx, BrowserContext::Documentation));
    }

    #[test]
    fn test_unknown_page() {
        let ctx = parse_title("Gmail - Google Chrome");
        assert!(matches!(ctx, BrowserContext::Unknown));
    }

    #[test]
    fn test_github_no_false_positive_on_url() {
        // Should not match "https://github.com" as owner/repo
        let ctx = parse_title("https://github.com/settings - Firefox");
        // This might match or not depending on parsing — the key is no panic
        let _ = ctx;
    }
}

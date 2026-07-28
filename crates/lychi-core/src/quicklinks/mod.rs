//! Quicklinks — user-defined parameterized commands.
//!
//! A quicklink maps a keyword to a *template* containing placeholders, plus a
//! [`QuicklinkKind`] saying what the expanded template IS:
//!
//! ```toml
//! [[commands.quicklinks]]
//! keyword  = "gh"
//! name     = "GitHub search"
//! kind     = "url"
//! template = "https://github.com/search?q={query}"
//!
//! [[commands.quicklinks]]
//! keyword  = "co"
//! name     = "Git checkout"
//! kind     = "shell"
//! template = "git checkout {branch}"
//! ```
//!
//! Then `gh tokio` opens a GitHub search and `co main` checks out a branch.
//! This is how a user grows Lychi without writing code.
//!
//! ## Why `kind` is stored, not sniffed
//!
//! It would be possible to guess ("starts with `http` → URL, `/` → path, else
//! shell"). That guess is a second decider living next to the real one, and it
//! decides something security-relevant: whether a string reaches a shell. A
//! stored `kind` is checked once at save time and then *known*, so the shell
//! gate keys off a fact rather than a heuristic.
//!
//! ## Escaping is per-kind, and that is the whole point
//!
//! The predecessor of this module (`bang`) URL-encoded every substitution
//! unconditionally, which is exactly right for a URL and exactly wrong for
//! everything else — `git checkout my%20branch` is not a branch. Each kind
//! escapes in its own domain:
//!
//! | Kind      | Escaping applied to substituted values |
//! |-----------|----------------------------------------|
//! | `Url`     | percent-encoding                       |
//! | `Shell`   | single-quote shell escaping            |
//! | `Open`    | none (path used verbatim)               |
//! | `Command` | none (re-enters Lychi's own router)     |
//!
//! Escaping applies ONLY to the user's runtime input, never to the template —
//! the template is authored by the user and its punctuation is meaningful.
//!
//! ## Security
//!
//! This module expands templates. It does not decide whether the result may
//! run — that is the job of the central deciders in [`crate::rules`], called by
//! the handler on the EXPANDED string. Expanding and authorizing are kept
//! separate so no caller can accidentally authorize a template and then run
//! something else.

use serde::{Deserialize, Serialize};

pub mod expand;
pub mod migrate;

pub use expand::{Expansion, PlaceholderError, expand_template, placeholders_in};

/// What an expanded quicklink template actually is — and therefore which
/// permission decider authorizes it and how its values are escaped.
///
/// Stored explicitly rather than inferred from the template text. See the
/// module docs for why.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, specta::Type, Default)]
#[serde(rename_all = "lowercase")]
pub enum QuicklinkKind {
    /// A web URL to navigate to. Values are percent-encoded.
    /// Authorized by [`crate::rules::uri::authorize_uri`].
    #[default]
    Url,
    /// A shell command line. Values are shell-quoted.
    /// Authorized by [`crate::rules::shell::authorize`] — on the EXPANDED
    /// string, because a template can expand into something its own text never
    /// showed.
    Shell,
    /// A filesystem path to open with the desktop's default handler.
    /// Authorized by [`crate::rules::path::authorize_path`].
    Open,
    /// Another Lychi command, re-entering the normal router (and therefore
    /// whatever gate that command already has).
    Command,
}

impl QuicklinkKind {
    /// The stable string form used in config and IPC.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Url => "url",
            Self::Shell => "shell",
            Self::Open => "open",
            Self::Command => "command",
        }
    }

    /// Parse from config/IPC text. Unknown values are rejected rather than
    /// defaulted: silently treating an unrecognised kind as `Url` would be a
    /// guess, and guessing about which gate applies is what `kind` exists to
    /// prevent.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "url" => Some(Self::Url),
            "shell" => Some(Self::Shell),
            "open" => Some(Self::Open),
            "command" => Some(Self::Command),
            _ => None,
        }
    }

    /// Whether this kind can reach a shell. Used to decide if the save-time UI
    /// should warn, and to pick the authorizing decider.
    pub fn is_shell(self) -> bool {
        matches!(self, Self::Shell)
    }
}

/// Longest accepted keyword — matches the AI-preset limit so the two
/// user-defined-keyword surfaces agree.
pub const MAX_KEYWORD: usize = 24;
/// Longest accepted display name.
pub const MAX_NAME: usize = 40;
/// Longest accepted template.
pub const MAX_TEMPLATE: usize = 2000;
/// Cap on how many quicklinks a user can define. Generous; exists so a
/// hand-edited or corrupted config can't produce an unbounded keyword table.
pub const MAX_QUICKLINKS: usize = 200;

/// One user-defined parameterized command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
pub struct Quicklink {
    /// The typed keyword, e.g. `gh`. Stored normalized (trimmed, lowercased)
    /// because the router matches case-insensitively.
    pub keyword: String,
    /// Human-readable label shown in the results row. Falls back to the
    /// keyword when empty.
    #[serde(default)]
    pub name: String,
    /// What the expanded template is.
    #[serde(default)]
    pub kind: QuicklinkKind,
    /// The template, containing `{named}` and/or legacy bare `{}` placeholders.
    pub template: String,
}

impl Quicklink {
    /// The label to show for this quicklink — its name, or the keyword when no
    /// name was given.
    pub fn display_name(&self) -> &str {
        if self.name.trim().is_empty() {
            &self.keyword
        } else {
            self.name.trim()
        }
    }

    /// Normalize a would-be keyword: trimmed and lowercased. Keywords are
    /// matched case-insensitively at the router, so they are stored canonically
    /// to keep collision checks and lookups consistent.
    pub fn normalize_keyword(keyword: &str) -> String {
        keyword.trim().to_ascii_lowercase()
    }

    /// Reason this quicklink is invalid, or `None` if it's fine.
    ///
    /// `is_reserved` reports whether a keyword collides with a built-in command
    /// prefix. It is injected by the caller because this module sits upstream
    /// of the action registry and must not depend on it — the same inversion
    /// the previous `search_engines` validation used.
    pub fn validation_error(&self, is_reserved: &dyn Fn(&str) -> bool) -> Option<String> {
        let keyword = Self::normalize_keyword(&self.keyword);
        if keyword.is_empty() {
            return Some("Keyword can't be empty".to_string());
        }
        if keyword.split_whitespace().count() != 1 {
            return Some("Keyword must be a single word (no spaces)".to_string());
        }
        if keyword.chars().count() > MAX_KEYWORD {
            return Some(format!("Keyword must be {MAX_KEYWORD} characters or fewer"));
        }
        if is_reserved(&keyword) {
            return Some(format!(
                "\"{keyword}\" is a reserved command and can't be used as a shortcut"
            ));
        }
        if self.name.chars().count() > MAX_NAME {
            return Some(format!("Name must be {MAX_NAME} characters or fewer"));
        }
        if self.template.trim().is_empty() {
            return Some("Template can't be empty".to_string());
        }
        if self.template.chars().count() > MAX_TEMPLATE {
            return Some(format!(
                "Template must be {MAX_TEMPLATE} characters or fewer"
            ));
        }
        if let Err(err) = expand::validate_template(&self.template) {
            return Some(err.to_string());
        }
        // A URL quicklink whose template isn't a URL is almost certainly a
        // mis-set kind — and the one mistake that would otherwise send a shell
        // string through the URL path, where the shell gate never runs.
        if self.kind == QuicklinkKind::Url && !looks_like_url(&self.template) {
            return Some(
                "A URL quicklink's template must start with http:// or https://".to_string(),
            );
        }
        None
    }
}

/// Whether a template is plausibly a web URL. Deliberately strict: this is used
/// to *reject* a mislabelled URL quicklink at save time, so a false "yes" is
/// worse than a false "no".
fn looks_like_url(template: &str) -> bool {
    let t = template.trim_start().to_ascii_lowercase();
    t.starts_with("http://") || t.starts_with("https://")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn never_reserved(_: &str) -> bool {
        false
    }

    fn ql(keyword: &str, kind: QuicklinkKind, template: &str) -> Quicklink {
        Quicklink {
            keyword: keyword.to_string(),
            name: String::new(),
            kind,
            template: template.to_string(),
        }
    }

    #[test]
    fn kind_round_trips_through_its_string_form() {
        for kind in [
            QuicklinkKind::Url,
            QuicklinkKind::Shell,
            QuicklinkKind::Open,
            QuicklinkKind::Command,
        ] {
            assert_eq!(QuicklinkKind::parse(kind.as_str()), Some(kind));
        }
    }

    #[test]
    fn unknown_kind_is_rejected_not_defaulted() {
        // Defaulting an unrecognised kind to Url would be a guess about which
        // permission gate applies — precisely what storing `kind` prevents.
        assert_eq!(QuicklinkKind::parse("exec"), None);
        assert_eq!(QuicklinkKind::parse(""), None);
    }

    #[test]
    fn kind_parsing_ignores_case_and_padding() {
        assert_eq!(QuicklinkKind::parse("  SHELL "), Some(QuicklinkKind::Shell));
    }

    #[test]
    fn reserved_keywords_are_refused() {
        let q = ql("open", QuicklinkKind::Url, "https://x.com/{query}");
        let err = q.validation_error(&|w| w == "open").unwrap();
        assert!(err.contains("reserved"), "got: {err}");
    }

    #[test]
    fn multi_word_keyword_is_refused() {
        let q = ql("my link", QuicklinkKind::Url, "https://x.com/{q}");
        assert!(
            q.validation_error(&never_reserved)
                .unwrap()
                .contains("single word")
        );
    }

    #[test]
    fn url_kind_demands_an_actual_url() {
        // The failure this guards: a shell string saved as kind=url would take
        // the navigate path and never meet the shell decider.
        let q = ql("bad", QuicklinkKind::Url, "rm -rf {path}");
        assert!(
            q.validation_error(&never_reserved)
                .unwrap()
                .contains("http")
        );
    }

    #[test]
    fn shell_kind_accepts_a_non_url_template() {
        let q = ql("co", QuicklinkKind::Shell, "git checkout {branch}");
        assert_eq!(q.validation_error(&never_reserved), None);
    }

    #[test]
    fn empty_template_is_refused() {
        let q = ql("x", QuicklinkKind::Shell, "   ");
        assert!(q.validation_error(&never_reserved).is_some());
    }

    #[test]
    fn malformed_placeholder_is_refused_at_save_time() {
        let q = ql("x", QuicklinkKind::Shell, "echo {unclosed");
        assert!(q.validation_error(&never_reserved).is_some());
    }

    #[test]
    fn display_name_falls_back_to_keyword() {
        let mut q = ql("gh", QuicklinkKind::Url, "https://x.com/{q}");
        assert_eq!(q.display_name(), "gh");
        q.name = "GitHub".to_string();
        assert_eq!(q.display_name(), "GitHub");
    }

    #[test]
    fn keyword_normalization_is_case_folding_and_trimming() {
        assert_eq!(Quicklink::normalize_keyword("  GH  "), "gh");
    }
}

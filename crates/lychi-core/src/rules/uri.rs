//! Central URI-scheme decider — the single authority for "may we hand this URI
//! to the desktop to open?". Mirrors the shell decider (`rules::shell::authorize`):
//! every place that opens a URI (`platform::open_uri`, and thus every `navigate`
//! result — web/yt/bang/bookmarks/url/file-open) funnels through `authorize_uri`,
//! so a dangerous scheme cannot slip in from any one source.
//!
//! The motivating case: browser bookmarks commonly hold `javascript:` bookmarklets
//! and can hold `data:`/`file:` URLs. Handed to `launch_default_for_uri` unfiltered
//! those execute a handler with the user's privileges. This decider blocks the
//! dangerous schemes at the one chokepoint they all pass through.

/// The verdict for opening a URI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UriDecision {
    /// Safe to open.
    Allow,
    /// Blocked — never open this scheme.
    Deny { reason: String },
}

/// Schemes we will open via the desktop's default handler. Deliberately small:
/// web links, mail, and local files/dirs — the things a launcher legitimately
/// opens. Everything else (and anything scheme-less that reached this far) is
/// denied.
const ALLOWED_SCHEMES: &[&str] = &["http", "https", "mailto", "file", "tel", "ftp", "ftps"];

/// Schemes we explicitly refuse, with a specific reason, because they execute
/// code or embed inline payloads rather than pointing at a resource.
const DENIED_SCHEMES: &[&str] = &[
    "javascript", // bookmarklets — run script in the default browser
    "data",       // inline payloads (data:text/html,<script>…)
    "vbscript",
    "blob",
];

/// The **central URI decider**. Extracts the scheme and decides Allow/Deny.
///
/// - Known-safe scheme (http/https/mailto/file/…) → `Allow`.
/// - Known-dangerous scheme (javascript/data/vbscript/blob) → `Deny`.
/// - Unknown or missing scheme → `Deny` (fail closed): callers that produce a
///   bare host must normalise to `https://…` BEFORE opening (the `url` handler
///   already does), so anything reaching here with no scheme is suspect.
pub fn authorize_uri(uri: &str) -> UriDecision {
    let trimmed = uri.trim();
    let scheme = scheme_of(trimmed);

    match scheme {
        Some(s) => {
            let s = s.to_ascii_lowercase();
            if DENIED_SCHEMES.contains(&s.as_str()) {
                return UriDecision::Deny {
                    reason: format!("Refusing to open a '{s}:' URI — that scheme can execute code"),
                };
            }
            if ALLOWED_SCHEMES.contains(&s.as_str()) {
                return UriDecision::Allow;
            }
            UriDecision::Deny {
                reason: format!("Refusing to open an unrecognised URI scheme '{s}:'"),
            }
        }
        None => UriDecision::Deny {
            reason: "Refusing to open a URI with no scheme".to_string(),
        },
    }
}

/// Convenience: `Ok(())` if allowed, `Err(reason)` if denied. For call sites that
/// return `Result`.
pub fn check_uri(uri: &str) -> Result<(), String> {
    match authorize_uri(uri) {
        UriDecision::Allow => Ok(()),
        UriDecision::Deny { reason } => Err(reason),
    }
}

/// Extract the scheme of a URI: the run of ASCII scheme chars before the first
/// `:`. Per RFC 3986 a scheme is `ALPHA *( ALPHA / DIGIT / "+" / "-" / "." )`.
/// Returns `None` if there's no `:` or the part before it isn't a valid scheme
/// (e.g. `example.com/path` — the `.` before any `:` disqualifies it, and a
/// path-only string has no scheme).
fn scheme_of(uri: &str) -> Option<&str> {
    let colon = uri.find(':')?;
    let candidate = &uri[..colon];
    if candidate.is_empty() {
        return None;
    }
    let mut chars = candidate.chars();
    let first = chars.next()?;
    if !first.is_ascii_alphabetic() {
        return None;
    }
    if chars.all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '.') {
        Some(candidate)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_web_and_local_schemes() {
        assert_eq!(authorize_uri("https://example.com"), UriDecision::Allow);
        assert_eq!(
            authorize_uri("http://example.com/x?y=1"),
            UriDecision::Allow
        );
        assert_eq!(authorize_uri("mailto:a@b.com"), UriDecision::Allow);
        assert_eq!(
            authorize_uri("file:///home/sab/notes.txt"),
            UriDecision::Allow
        );
        // Case-insensitive scheme.
        assert_eq!(authorize_uri("HTTPS://EXAMPLE.COM"), UriDecision::Allow);
    }

    #[test]
    fn denies_code_executing_schemes() {
        for uri in [
            "javascript:alert(1)",
            "JavaScript:void(0)",
            "data:text/html,<script>alert(1)</script>",
            "vbscript:msgbox(1)",
            "blob:https://x/abc",
        ] {
            assert!(
                matches!(authorize_uri(uri), UriDecision::Deny { .. }),
                "should deny: {uri}"
            );
        }
    }

    #[test]
    fn denies_unknown_and_schemeless() {
        assert!(matches!(
            authorize_uri("chrome://settings"),
            UriDecision::Deny { .. }
        ));
        assert!(matches!(
            authorize_uri("steam://run/1"),
            UriDecision::Deny { .. }
        ));
        // No scheme → fail closed (callers must normalise bare hosts first).
        assert!(matches!(
            authorize_uri("example.com/path"),
            UriDecision::Deny { .. }
        ));
        assert!(matches!(
            authorize_uri("/just/a/path"),
            UriDecision::Deny { .. }
        ));
        assert!(matches!(authorize_uri(""), UriDecision::Deny { .. }));
    }

    #[test]
    fn scheme_of_parses_correctly() {
        assert_eq!(scheme_of("https://x"), Some("https"));
        assert_eq!(scheme_of("mailto:x"), Some("mailto"));
        assert_eq!(scheme_of("example.com/a:b"), None); // dot before colon
        assert_eq!(scheme_of("/path"), None);
        assert_eq!(scheme_of("noColon"), None);
    }

    #[test]
    fn check_uri_bridges_to_result() {
        assert!(check_uri("https://x").is_ok());
        assert!(check_uri("javascript:x").is_err());
    }
}

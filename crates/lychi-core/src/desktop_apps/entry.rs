use std::sync::OnceLock;

/// A parsed and pre-indexed desktop application entry.
#[derive(Debug)]
pub struct DesktopEntry {
    /// Display name (e.g. "Visual Studio Code")
    pub name: String,
    /// Raw Exec= value with %u/%f placeholders stripped
    pub exec: String,
    /// Basename of the executable (e.g. "code" from "/usr/bin/code")
    pub exec_basename: String,
    /// StartupWMClass= (e.g. "Code")
    pub wm_class: Option<String>,
    /// GenericName= (e.g. "Web Browser", "Text Editor")
    pub generic_name: Option<String>,
    /// Keywords= normalized: lowercase, split on ';'/whitespace, deduped, stopwords removed
    pub keywords: Vec<String>,
    /// Lowercase tokens from Name (len ≥ 3, stopwords removed)
    pub name_tokens: Vec<String>,
    /// Acronym from Name initials (e.g. "vsc" from "Visual Studio Code")
    pub acronym: String,
    /// Icon name or path
    pub icon: Option<String>,
    /// Absolute path to the .desktop file — used as stable canonical ID
    pub desktop_path: String,
    /// Resolved icon filesystem path — populated at warmup
    pub icon_path: OnceLock<Option<String>>,
}

/// Tokens and stopwords to skip when building name_tokens / keyword index.
pub const STOPWORDS: &[&str] = &[
    "app",
    "application",
    "applications",
    "desktop",
    "software",
    "program",
    "the",
    "and",
    "for",
    "of",
    "to",
    "a",
    "an",
    "in",
    "on",
    "at",
    "by",
    "x",
    "de",
    "gtk",
    "qt",
];

/// Minimum token length to index.
pub const MIN_TOKEN_LEN: usize = 3;

/// Normalize a query string for scoring comparisons: lowercase + trim.
pub fn query_norm(s: &str) -> String {
    s.trim().to_lowercase()
}

/// Tokenize a string into indexable tokens (lowercase, deduped, stopwords removed, len ≥ MIN).
pub fn tokenize(s: &str) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    s.split(|c: char| c == ';' || c.is_whitespace() || c == '-' || c == '_')
        .map(|t| {
            t.trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase()
        })
        .filter(|t| {
            t.len() >= MIN_TOKEN_LEN && !STOPWORDS.contains(&t.as_str()) && seen.insert(t.clone())
        })
        .collect()
}

/// Derive an acronym from initials of whitespace-separated words.
/// "Visual Studio Code" → "vsc"
pub fn make_acronym(name: &str) -> String {
    name.split_whitespace()
        .filter_map(|w| w.chars().next())
        .collect::<String>()
        .to_lowercase()
}

/// Strip all .desktop field codes (%U %u %F %f %i %c %k %d %D %n %N %v %m).
pub fn strip_field_codes(exec: &str) -> String {
    // Field codes are always %<letter> — replace them and collapse extra whitespace
    let mut out = String::with_capacity(exec.len());
    let mut chars = exec.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '%' {
            // Skip the next character (the field code letter)
            chars.next();
        } else {
            out.push(ch);
        }
    }
    // Collapse runs of whitespace to single space and trim
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Extract the basename of an Exec= path, skipping `env` + `KEY=VALUE` prefix tokens.
///
/// Handles common patterns:
/// - "/usr/bin/code %U"                          → "code"
/// - "env BAMF_HINT=/foo /usr/bin/code --flag"   → "code"
/// - "code"                                       → "code"
/// - "\"code\" --new-window"                      → "code"
pub fn exec_basename(exec: &str) -> String {
    let cleaned = strip_field_codes(exec);
    // Tokenize on whitespace, find the first token that looks like a command
    // (not "env", not a KEY=VALUE assignment)
    let cmd = cleaned
        .split_whitespace()
        .find(|t| {
            let t = t.trim_matches('"');
            t != "env" && !t.contains('=')
        })
        .unwrap_or(&cleaned);

    let cmd = cmd.trim_matches('"');
    std::path::Path::new(cmd)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(cmd)
        .to_lowercase()
}

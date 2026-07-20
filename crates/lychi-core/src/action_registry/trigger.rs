//! Handler triggers — how a handler declares the keyword prefixes that route to
//! it, and how its arguments are shaped.
//!
//! Before this, the prefix list (`KNOWN_PREFIXES`) and a ~140-arm match encoding
//! every handler's argument transformation lived centrally in `intent/patterns.rs`
//! — so adding a keyword-triggered handler meant editing that central file, and
//! the list could silently drift from the handler set. Now each handler declares
//! its own `triggers()`, the registry builds the routing index from them, and
//! `patterns.rs` keeps only genuinely *structural* detection (URLs, paths, math).
//!
//! Most handlers just pass their args through unchanged; a handful reshape them
//! (strip a filler word, prepend a fixed verb). Those common shapes are covered
//! by `ArgTransform` so the declaration stays data, not code. The rare truly
//! conditional cases (e.g. `open <url>` redirecting to the url handler) remain
//! structural rules in `patterns.rs` — they aren't a single handler's concern.

/// How to turn the text after a matched prefix into the handler's args.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArgTransform {
    /// Args pass through unchanged. `web rust` → handler "web" with args "rust".
    PassThrough,
    /// Ignore the args entirely and use this fixed string. `reboot` → "reboot".
    Fixed(&'static str),
    /// Prepend a fixed verb + space. `password 20` → "password 20" for the
    /// generate handler (verb "password"). Empty args yield just the verb.
    Prepend(&'static str),
    /// Prepend the matched keyword itself + space. `base64 hi` → "base64 hi" for
    /// the devutil handler, which dispatches on the leading verb.
    PrependKeyword,
    /// Strip a leading filler word (+ trailing space) if present, else pass
    /// through. `weather in tokyo` → "tokyo" (strip "in ").
    StripLeading(&'static str),
    /// Ignore the args and use the matched keyword itself. `cpu extra junk` →
    /// "cpu" for the sysinfo handler (bare-word system shortcuts).
    KeywordOnly,
}

impl ArgTransform {
    /// Apply the transform to `(keyword, rest)` where `keyword` is the matched
    /// prefix and `rest` is the trimmed remainder.
    pub fn apply(&self, keyword: &str, rest: &str) -> String {
        match self {
            ArgTransform::PassThrough => rest.to_string(),
            ArgTransform::Fixed(s) => s.to_string(),
            ArgTransform::Prepend(verb) => {
                if rest.is_empty() {
                    verb.to_string()
                } else {
                    format!("{verb} {rest}")
                }
            }
            ArgTransform::PrependKeyword => {
                if rest.is_empty() {
                    keyword.to_string()
                } else {
                    format!("{keyword} {rest}")
                }
            }
            ArgTransform::StripLeading(prefix) => rest
                .strip_prefix(prefix)
                .map(str::trim_start)
                .unwrap_or(rest)
                .to_string(),
            ArgTransform::KeywordOnly => keyword.to_string(),
        }
    }
}

/// One routing trigger: the keyword prefixes that map to a handler, and how to
/// shape the args. A handler can declare several (e.g. an alias group with a
/// different transform).
#[derive(Debug, Clone)]
pub struct Trigger {
    /// Keyword(s) that route here, matched case-insensitively as the first word.
    pub prefixes: &'static [&'static str],
    /// How the remaining text becomes the handler's args.
    pub transform: ArgTransform,
}

impl Trigger {
    /// A trigger for one or more keyword prefixes, args unchanged (the common
    /// case). Pass a slice literal: `Trigger::keywords(&["note", "notes"])`.
    pub const fn keywords(prefixes: &'static [&'static str]) -> Self {
        Self {
            prefixes,
            transform: ArgTransform::PassThrough,
        }
    }

    /// A trigger with an explicit arg transform.
    pub const fn new(prefixes: &'static [&'static str], transform: ArgTransform) -> Self {
        Self {
            prefixes,
            transform,
        }
    }
}

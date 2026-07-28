//! Template expansion — turning `git checkout {branch}` plus `main` into
//! `git checkout main`, with escaping chosen by the quicklink's kind.
//!
//! ## The rule that matters
//!
//! Escaping is applied to the **substituted values only**, never to the
//! template. The template is authored by the user and its punctuation is
//! meaningful — quoting it would break every URL query string and every shell
//! pipeline. The runtime input is *not* authored at save time, so it is the
//! part that gets escaped.
//!
//! This is the inverse of the old `bang` behaviour, which URL-encoded
//! everything unconditionally. That was correct for URLs and silently wrong for
//! anything else.
//!
//! ## Escaping does not replace authorization
//!
//! Shell-quoting a value stops it from *adding* shell syntax (`; rm -rf ~`
//! becomes a literal argument). It does not make the resulting command safe —
//! the template itself may be destructive. Authorization is a separate step,
//! run by the caller on the expanded string via [`crate::rules`]. Expansion and
//! authorization are deliberately not fused, so no caller can authorize one
//! string and run another.

use std::fmt;

use super::QuicklinkKind;

/// A template that could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlaceholderError {
    /// A `{` with no matching `}`.
    Unclosed,
    /// A `{name}` whose name contains characters that aren't allowed.
    BadName(String),
}

impl fmt::Display for PlaceholderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unclosed => write!(f, "Template has an unclosed '{{' placeholder"),
            Self::BadName(n) => write!(
                f,
                "Placeholder \"{{{n}}}\" is invalid — use letters, digits, - or _"
            ),
        }
    }
}

impl std::error::Error for PlaceholderError {}

/// The result of expanding a template against user input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expansion {
    /// The fully substituted string, escaped per the quicklink's kind.
    pub text: String,
    /// Placeholders that received no input because the user supplied fewer
    /// arguments than the template has slots. Substituted as empty; reported so
    /// a caller can prompt or show a hint rather than silently running a
    /// half-filled command.
    pub missing: Vec<String>,
}

/// Parse a template into its literal and placeholder segments.
///
/// Returns the segment list so both expansion and validation walk the exact
/// same parser — a template that validates is a template that expands.
fn parse(template: &str) -> Result<Vec<Segment>, PlaceholderError> {
    let mut out = Vec::new();
    let mut literal = String::new();
    let mut rest = template;

    while let Some(open) = rest.find('{') {
        literal.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            return Err(PlaceholderError::Unclosed);
        };
        let name = &after[..close];

        if name.is_empty() {
            // Legacy bare `{}` — the whole remaining input.
            if !literal.is_empty() {
                out.push(Segment::Literal(std::mem::take(&mut literal)));
            }
            out.push(Segment::Rest);
        } else if is_valid_name(name) {
            if !literal.is_empty() {
                out.push(Segment::Literal(std::mem::take(&mut literal)));
            }
            out.push(Segment::Named(name.to_string()));
        } else {
            return Err(PlaceholderError::BadName(name.to_string()));
        }
        rest = &after[close + 1..];
    }

    literal.push_str(rest);
    if !literal.is_empty() {
        out.push(Segment::Literal(literal));
    }
    Ok(out)
}

/// Placeholder names are restricted so a typo like `{my branch}` is caught at
/// save time rather than producing a literal that never substitutes.
fn is_valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Segment {
    Literal(String),
    /// A named placeholder, `{branch}`.
    Named(String),
    /// Legacy `{}` — consumes all remaining input.
    Rest,
}

/// Check that a template parses, without expanding it. Used on the save path so
/// a malformed template is rejected with a message instead of failing at run
/// time.
pub fn validate_template(template: &str) -> Result<(), PlaceholderError> {
    parse(template).map(|_| ())
}

/// The placeholder names a template uses, in order, with `{}` reported as
/// `"…"`. Used by the Settings UI to show what a quicklink expects.
pub fn placeholders_in(template: &str) -> Vec<String> {
    parse(template)
        .map(|segs| {
            segs.into_iter()
                .filter_map(|s| match s {
                    Segment::Named(n) => Some(n),
                    Segment::Rest => Some("…".to_string()),
                    Segment::Literal(_) => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Expand `template` against `input`, escaping substituted values per `kind`.
///
/// Argument binding, chosen to make the common cases do the obvious thing:
/// - Exactly one placeholder → it receives the entire input, spaces and all.
///   So `gh tokio async runtime` searches the whole phrase, not just `tokio`.
/// - Several placeholders → input splits on whitespace positionally, and the
///   LAST placeholder absorbs the remainder. So `commit fix the parser bug`
///   with `git commit -m {message}` keeps the message intact, and
///   `co {branch} {message}` binds `branch` to one word and the rest to
///   `message`.
/// - A bare `{}` always absorbs everything remaining.
///
/// Missing arguments substitute empty and are reported in
/// [`Expansion::missing`] rather than erroring — a partially typed quicklink
/// should still preview, and the caller decides whether to run it.
pub fn expand_template(
    template: &str,
    input: &str,
    kind: QuicklinkKind,
) -> Result<Expansion, PlaceholderError> {
    let segments = parse(template)?;
    let input = input.trim();

    let slots: Vec<&Segment> = segments
        .iter()
        .filter(|s| matches!(s, Segment::Named(_) | Segment::Rest))
        .collect();

    // Bind each slot to a piece of the input.
    let mut values: Vec<String> = Vec::with_capacity(slots.len());
    if slots.len() <= 1 {
        values.push(input.to_string());
    } else {
        let mut remaining = input;
        for (i, slot) in slots.iter().enumerate() {
            let is_last = i + 1 == slots.len();
            // `{}` and the final slot take everything that's left.
            if is_last || matches!(slot, Segment::Rest) {
                values.push(remaining.trim().to_string());
                remaining = "";
            } else {
                match remaining.trim_start().split_once(char::is_whitespace) {
                    Some((word, rest)) => {
                        values.push(word.to_string());
                        remaining = rest;
                    }
                    None => {
                        values.push(remaining.trim().to_string());
                        remaining = "";
                    }
                }
            }
        }
    }

    let mut text = String::with_capacity(template.len() + input.len());
    let mut missing = Vec::new();
    let mut slot_index = 0;

    for segment in &segments {
        match segment {
            Segment::Literal(lit) => text.push_str(lit),
            Segment::Named(name) => {
                let raw = values.get(slot_index).map(String::as_str).unwrap_or("");
                if raw.is_empty() {
                    missing.push(name.clone());
                }
                text.push_str(&escape(raw, kind));
                slot_index += 1;
            }
            Segment::Rest => {
                let raw = values.get(slot_index).map(String::as_str).unwrap_or("");
                text.push_str(&escape(raw, kind));
                slot_index += 1;
            }
        }
    }

    // A template with no placeholders at all still accepts input: append it,
    // matching how aliases and AI presets behave when the template is a bare
    // prefix. Keeps `so = "https://stackoverflow.com/search?q="` working.
    if slots.is_empty() && !input.is_empty() {
        text.push_str(&escape(input, kind));
    }

    Ok(Expansion { text, missing })
}

/// Escape one substituted value for the domain it is about to land in.
///
/// This is the function the whole module exists for. Getting it wrong in either
/// direction is a real bug: too little escaping lets runtime input inject
/// syntax, too much corrupts legitimate values.
fn escape(value: &str, kind: QuicklinkKind) -> String {
    match kind {
        // Percent-encode: the value lands in a query string.
        QuicklinkKind::Url => urlencoding::encode(value).into_owned(),
        // Single-quote: the value lands on a shell command line, where
        // whitespace, `;`, `|`, `$` and friends would otherwise be syntax.
        QuicklinkKind::Shell => shell_quote(value),
        // A path or a Lychi command — neither is re-parsed by a shell, and
        // quoting would corrupt them. `~/My Notes` must stay `~/My Notes`.
        QuicklinkKind::Open | QuicklinkKind::Command => value.to_string(),
    }
}

/// POSIX single-quote escaping: wrap in `'…'` and replace each embedded `'`
/// with `'\''`. Inside single quotes the shell interprets nothing, so this is
/// total for a single argument — no expansion, no word splitting, no
/// substitution.
///
/// Empty input becomes `''` so the argument still exists positionally rather
/// than vanishing from the command line.
fn shell_quote(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('\'');
    for ch in value.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expand(template: &str, input: &str, kind: QuicklinkKind) -> String {
        expand_template(template, input, kind).unwrap().text
    }

    // ---- placeholder parsing -------------------------------------------

    #[test]
    fn unclosed_placeholder_is_an_error() {
        assert_eq!(
            validate_template("echo {branch"),
            Err(PlaceholderError::Unclosed)
        );
    }

    #[test]
    fn placeholder_with_a_space_is_rejected() {
        // Caught at save time, rather than silently never substituting.
        assert!(matches!(
            validate_template("echo {my branch}"),
            Err(PlaceholderError::BadName(_))
        ));
    }

    #[test]
    fn reports_the_placeholders_a_template_uses() {
        assert_eq!(
            placeholders_in("git commit -m {message} --author {who}"),
            vec!["message", "who"]
        );
        assert_eq!(placeholders_in("https://x.com/?q={}"), vec!["…"]);
    }

    // ---- argument binding ----------------------------------------------

    #[test]
    fn a_single_placeholder_absorbs_the_whole_input() {
        // `gh tokio async runtime` should search the phrase, not just "tokio".
        assert_eq!(
            expand(
                "https://gh.com/?q={query}",
                "tokio async runtime",
                QuicklinkKind::Url
            ),
            "https://gh.com/?q=tokio%20async%20runtime"
        );
    }

    #[test]
    fn multiple_placeholders_split_positionally_and_last_takes_the_rest() {
        assert_eq!(
            expand(
                "git commit {flag} -m {message}",
                "-a fix the parser bug",
                QuicklinkKind::Shell
            ),
            "git commit '-a' -m 'fix the parser bug'"
        );
    }

    #[test]
    fn legacy_bare_placeholder_still_works() {
        // The seven shipped built-ins and every existing user config rely on
        // this spelling; breaking it would be a silent regression.
        assert_eq!(
            expand(
                "https://maps.google.com/?q={}&z=10",
                "eiffel tower",
                QuicklinkKind::Url
            ),
            "https://maps.google.com/?q=eiffel%20tower&z=10"
        );
    }

    #[test]
    fn a_template_without_placeholders_appends_the_input() {
        // Matches the old bang behaviour for prefix-style templates.
        assert_eq!(
            expand(
                "https://so.com/search?q=",
                "rust lifetimes",
                QuicklinkKind::Url
            ),
            "https://so.com/search?q=rust%20lifetimes"
        );
    }

    #[test]
    fn missing_arguments_are_reported_not_guessed() {
        let e = expand_template("git commit {flag} -m {msg}", "", QuicklinkKind::Shell).unwrap();
        assert_eq!(e.missing, vec!["flag", "msg"]);
    }

    #[test]
    fn no_input_still_expands_a_placeholderless_template() {
        assert_eq!(expand("git status", "", QuicklinkKind::Shell), "git status");
    }

    // ---- escaping: the security-relevant half --------------------------

    #[test]
    fn shell_values_are_quoted_so_input_cannot_inject_syntax() {
        // The core injection case. Without quoting this would be TWO commands.
        let out = expand(
            "git checkout {branch}",
            "main; rm -rf ~",
            QuicklinkKind::Shell,
        );
        assert_eq!(out, "git checkout 'main; rm -rf ~'");
        // The dangerous text survives only as a literal argument: the whole
        // value sits inside one quoted span, so the shell sees no `;` operator.
        // (Asserting the `;` is absent would be wrong — a *quoted* semicolon is
        // exactly what safe escaping produces.)
        let quoted = out.split_once('\'').unwrap().1.rsplit_once('\'').unwrap().0;
        assert_eq!(quoted, "main; rm -rf ~");
    }

    #[test]
    fn embedded_single_quotes_cannot_break_out_of_the_quoting() {
        // The classic escape: input containing a quote must not terminate ours.
        let out = expand("echo {msg}", "it's; rm -rf ~", QuicklinkKind::Shell);
        assert_eq!(out, r"echo 'it'\''s; rm -rf ~'");
    }

    #[test]
    fn shell_values_neutralize_substitution_and_pipes() {
        assert_eq!(
            expand("echo {x}", "$(whoami) | tee /tmp/x", QuicklinkKind::Shell),
            "echo '$(whoami) | tee /tmp/x'"
        );
    }

    #[test]
    fn the_template_itself_is_never_escaped() {
        // The user authored the template; its `|` is a deliberate pipeline and
        // must survive. Only the substituted value is quoted.
        assert_eq!(
            expand("cat {file} | wc -l", "notes.txt", QuicklinkKind::Shell),
            "cat 'notes.txt' | wc -l"
        );
    }

    #[test]
    fn url_values_are_percent_encoded_but_the_template_is_not() {
        // `?` and `&` in the template are structure; in the value they are data.
        assert_eq!(
            expand("https://x.com/s?q={q}&lang=en", "a&b c", QuicklinkKind::Url),
            "https://x.com/s?q=a%26b%20c&lang=en"
        );
    }

    #[test]
    fn paths_are_not_quoted_or_encoded() {
        // The old bang code percent-encoded unconditionally, which turned
        // `~/My Notes` into `~%2FMy%20Notes` — a path that does not exist.
        assert_eq!(
            expand("~/projects/{name}", "My Notes", QuicklinkKind::Open),
            "~/projects/My Notes"
        );
    }

    #[test]
    fn lychi_commands_are_passed_through_verbatim() {
        assert_eq!(
            expand("note add {text}", "buy milk & eggs", QuicklinkKind::Command),
            "note add buy milk & eggs"
        );
    }

    #[test]
    fn an_empty_shell_value_stays_a_positional_argument() {
        // `''` keeps the argument present; dropping it would silently shift
        // every later argument left.
        assert_eq!(
            expand("grep {pat} file", "", QuicklinkKind::Shell),
            "grep '' file"
        );
    }

    #[test]
    fn unicode_values_survive_every_kind_intact() {
        assert_eq!(
            expand("echo {x}", "café ☕", QuicklinkKind::Shell),
            "echo 'café ☕'"
        );
        assert_eq!(expand("~/{x}", "café ☕", QuicklinkKind::Open), "~/café ☕");
    }
}

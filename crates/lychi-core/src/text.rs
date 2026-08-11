//! Display-text helpers shared by every handler that renders a row.
//!
//! **Why this module exists.** Truncating a label for display was reimplemented
//! seven times across `handlers/` and `context/`. Four of those copies sliced
//! by byte index (`&s[..max - 3]`) and **panicked** the moment the text crossed
//! a UTF-8 char boundary — which window titles, clipboard contents and
//! bookmark URLs do routinely (any app can set a title, and music players use
//! emoji as a matter of course). The other three were already correct but each
//! in its own way. One decider, one behaviour.

/// The ellipsis appended to truncated display text.
///
/// Public because a few call sites must recognise a truncated label they
/// produced earlier (e.g. resolving a completion row back to its window). Those
/// sites must strip *this* constant rather than hardcode a literal — a
/// hardcoded `"..."` silently stopped matching the moment this changed.
pub const ELLIPSIS: char = '…';

/// Truncate `text` to at most `max_chars` **characters** for display, appending
/// `…` when anything was removed. The returned string is never longer than
/// `max_chars` characters.
///
/// Counting characters rather than bytes is deliberate: these caps size a
/// launcher row, and a byte cap would show a Japanese or emoji title at a third
/// of the intended width while an ASCII one filled the row.
///
/// Never panics: the split is by character, so a multi-byte boundary cannot be
/// hit. `max_chars == 0` yields an empty string.
pub fn truncate_display(text: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    // Cheap accept: `len()` is an upper bound on the character count, so text
    // that fits in bytes certainly fits in chars and needs no counting.
    if text.len() <= max_chars {
        return text.to_string();
    }
    if text.chars().count() <= max_chars {
        return text.to_string();
    }

    // Reserve one character for the ellipsis so the result respects the cap.
    let keep = max_chars - 1;
    let mut out: String = text.chars().take(keep).collect();
    // Trailing whitespace before an ellipsis reads as a typo.
    let trimmed = out.trim_end();
    if trimmed.len() != out.len() {
        out.truncate(trimmed.len());
    }
    out.push(ELLIPSIS);
    out
}

/// Truncate the **first line** of `text` for display. Used for values that may
/// carry newlines (clipboard entries, snippet bodies) where a row must stay one
/// line regardless of the cap.
pub fn truncate_first_line(text: &str, max_chars: usize) -> String {
    truncate_display(text.lines().next().unwrap_or(text), max_chars)
}

/// Replace obvious secret shapes in a command line with `[redacted]`, for the
/// few log lines that must carry command text (shell security decisions at
/// debug/warn). The log file is the artifact beta users send in with bug
/// reports, so a `run curl -H 'Authorization: Bearer sk-…'` must not land in
/// it verbatim even at levels above the default.
///
/// Deliberately a conservative token scanner, not a regex engine: it redacts
/// whitespace-delimited tokens that carry a known secret PREFIX (API keys,
/// PATs, JWTs), the value of `key=value` pairs whose key names a credential,
/// and the token following a `Bearer` marker. It will miss exotic shapes —
/// that is acceptable for a defense-in-depth layer whose primary protection is
/// that full command text only appears below the default log level at all.
///
/// Rejoins tokens with single spaces: the output is for log lines, where exact
/// whitespace carries no meaning.
pub fn scrub_secrets(cmd: &str) -> String {
    const SECRET_PREFIXES: [&str; 10] = [
        "sk-",         // OpenAI/Anthropic-style API keys
        "sk_live_",    // Stripe live
        "sk_test_",    // Stripe test
        "ghp_",        // GitHub PAT (classic)
        "gho_",        // GitHub OAuth
        "github_pat_", // GitHub PAT (fine-grained)
        "xoxb-",       // Slack bot token
        "xoxp-",       // Slack user token
        "AKIA",        // AWS access key id
        "AIza",        // Google API key
    ];
    const SECRET_KEYS: [&str; 9] = [
        "token",
        "access_token",
        "api_key",
        "apikey",
        "secret",
        "password",
        "passwd",
        "auth",
        "authorization",
    ];

    let mut out: Vec<String> = Vec::new();
    let mut redact_next = false;
    for word in cmd.split_whitespace() {
        // Strip surrounding quotes for classification, keep them in output.
        let bare = word.trim_matches(|c| c == '\'' || c == '"');

        if redact_next {
            redact_next = false;
            out.push("[redacted]".into());
            continue;
        }
        if bare.eq_ignore_ascii_case("bearer") {
            redact_next = true;
            out.push(word.to_string());
            continue;
        }
        if SECRET_PREFIXES
            .iter()
            .any(|p| bare.starts_with(p) && bare.len() >= p.len() + 8)
        {
            out.push("[redacted]".into());
            continue;
        }
        if let Some((key, value)) = bare.split_once('=')
            && !value.is_empty()
            && SECRET_KEYS
                .iter()
                .any(|k| key.to_ascii_lowercase().trim_start_matches("--") == *k)
        {
            out.push(format!("{key}=[redacted]"));
            continue;
        }
        out.push(word.to_string());
    }
    out.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrub_redacts_bearer_tokens_and_key_prefixes() {
        assert_eq!(
            scrub_secrets(
                "curl -H 'Authorization: Bearer sk-abc123def456' https://api.example.com"
            ),
            "curl -H 'Authorization: Bearer [redacted] https://api.example.com"
        );
        assert_eq!(
            scrub_secrets("deploy --key ghp_0123456789abcdef"),
            "deploy --key [redacted]"
        );
    }

    #[test]
    fn scrub_redacts_credential_key_value_pairs() {
        assert_eq!(
            scrub_secrets("run TOKEN=supersecret ./deploy.sh"),
            "run TOKEN=[redacted] ./deploy.sh"
        );
        assert_eq!(
            scrub_secrets("mysql --password=hunter2 -u root"),
            "mysql --password=[redacted] -u root"
        );
    }

    #[test]
    fn scrub_leaves_ordinary_commands_alone() {
        assert_eq!(scrub_secrets("ls -la /tmp"), "ls -la /tmp");
        // A short sk- token is more likely a flag than a key.
        assert_eq!(scrub_secrets("git log --sk-ip"), "git log --sk-ip");
        // key=value with a non-credential key survives.
        assert_eq!(scrub_secrets("make MODE=release"), "make MODE=release");
    }

    #[test]
    fn short_text_is_returned_unchanged() {
        assert_eq!(truncate_display("hello", 10), "hello");
        assert_eq!(truncate_display("", 10), "");
    }

    #[test]
    fn exact_fit_is_not_truncated() {
        assert_eq!(truncate_display("abcde", 5), "abcde");
    }

    #[test]
    fn overflow_gets_an_ellipsis_within_the_cap() {
        let out = truncate_display("abcdefghij", 5);
        assert_eq!(out, "abcd…");
        assert_eq!(out.chars().count(), 5);
    }

    /// The regression this module exists for: a naive `&s[..max - 3]` panics
    /// here with "byte index is not a char boundary".
    #[test]
    fn multibyte_text_never_panics() {
        let title = "🎵🎵🎵🎵🎵🎵🎵🎵🎵🎵 Now Playing";
        for cap in 0..=title.chars().count() + 5 {
            let out = truncate_display(title, cap);
            assert!(out.chars().count() <= cap);
        }
    }

    #[test]
    fn multibyte_is_cut_by_character_not_byte() {
        // 10 emoji = 40 bytes. A byte cap would show 7; a char cap shows 9 + ….
        let out = truncate_display("🎵🎵🎵🎵🎵🎵🎵🎵🎵🎵 Now Playing", 10);
        assert_eq!(out, "🎵🎵🎵🎵🎵🎵🎵🎵🎵…");
        assert_eq!(out.chars().count(), 10);
    }

    #[test]
    fn cjk_is_cut_by_character() {
        let out = truncate_display("日本語のテキストです", 5);
        assert_eq!(out, "日本語の…");
        assert_eq!(out.chars().count(), 5);
    }

    #[test]
    fn combining_characters_do_not_panic() {
        // e + combining acute, repeated — chars() splits these apart, but the
        // only contract that matters here is "does not panic, respects cap".
        let s = "e\u{301}".repeat(20);
        let out = truncate_display(&s, 7);
        assert!(out.chars().count() <= 7);
    }

    #[test]
    fn zero_cap_is_empty() {
        assert_eq!(truncate_display("anything", 0), "");
        assert_eq!(truncate_display("🎵", 0), "");
    }

    #[test]
    fn cap_of_one_is_just_the_ellipsis() {
        assert_eq!(truncate_display("abcdef", 1), "…");
    }

    #[test]
    fn trailing_space_is_trimmed_before_the_ellipsis() {
        assert_eq!(truncate_display("ab cdef", 4), "ab…");
    }

    #[test]
    fn first_line_stops_at_the_newline() {
        assert_eq!(truncate_first_line("one\ntwo\nthree", 40), "one");
        assert_eq!(truncate_first_line("no newline", 40), "no newline");
    }

    #[test]
    fn first_line_still_respects_the_cap() {
        let out = truncate_first_line("aaaaaaaaaaaaaaa\nsecond", 5);
        assert_eq!(out, "aaaa…");
    }

    #[test]
    fn first_line_of_multibyte_does_not_panic() {
        let out = truncate_first_line("🎵🎵🎵🎵🎵\nnext", 3);
        assert_eq!(out.chars().count(), 3);
    }

    /// Output is compared against typed input at `clipboard.rs` (the
    /// `args.starts_with(truncate_label(..))` prefix check), so ASCII
    /// behaviour must stay predictable.
    #[test]
    fn ascii_output_is_a_prefix_of_the_input() {
        let s = "the quick brown fox jumps";
        let out = truncate_display(s, 10);
        let body = out.trim_end_matches(ELLIPSIS);
        assert!(s.starts_with(body), "{out:?} not derived from {s:?}");
    }
}

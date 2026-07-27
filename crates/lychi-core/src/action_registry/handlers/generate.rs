use async_trait::async_trait;

use crate::action_registry::{
    ActionHandler, ActionResult, CommandCategory, CompletionItem, ExecContext, OutputType,
};
use crate::error::LychiError;

use super::clipboard::write_to_clipboard;

pub struct GenerateHandler;

impl Default for GenerateHandler {
    fn default() -> Self {
        Self::new()
    }
}

impl GenerateHandler {
    pub fn new() -> Self {
        Self
    }
}

/// Character classes for password generation.
const UPPER: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ";
const LOWER: &[u8] = b"abcdefghijklmnopqrstuvwxyz";
const DIGITS: &[u8] = b"0123456789";
const SYMBOLS: &[u8] = b"!@#$%^&*()-_=+[]{}|;:,.<>?";

/// Generate a random password with guaranteed character class coverage.
fn generate_password(length: usize) -> Result<String, LychiError> {
    if !(8..=128).contains(&length) {
        return Err(LychiError::ExecutionFailed(
            "Password length must be between 8 and 128".to_string(),
        ));
    }

    let alphabet: Vec<u8> = [UPPER, LOWER, DIGITS, SYMBOLS].concat();
    let mut buf = vec![0u8; length];
    getrandom::fill(&mut buf)
        .map_err(|e| LychiError::ExecutionFailed(format!("Random generation failed: {e}")))?;

    // Map random bytes to alphabet characters
    let mut password: Vec<u8> = buf
        .iter()
        .map(|b| alphabet[*b as usize % alphabet.len()])
        .collect();

    // Guarantee one character from each class in first 4 positions
    let mut class_buf = [0u8; 4];
    getrandom::fill(&mut class_buf)
        .map_err(|e| LychiError::ExecutionFailed(format!("Random generation failed: {e}")))?;
    password[0] = UPPER[class_buf[0] as usize % UPPER.len()];
    password[1] = LOWER[class_buf[1] as usize % LOWER.len()];
    password[2] = DIGITS[class_buf[2] as usize % DIGITS.len()];
    password[3] = SYMBOLS[class_buf[3] as usize % SYMBOLS.len()];

    // Fisher-Yates shuffle to distribute guaranteed chars
    let mut shuffle_buf = vec![0u8; length];
    getrandom::fill(&mut shuffle_buf)
        .map_err(|e| LychiError::ExecutionFailed(format!("Random generation failed: {e}")))?;
    for i in (1..length).rev() {
        let j = shuffle_buf[i] as usize % (i + 1);
        password.swap(i, j);
    }

    Ok(String::from_utf8(password).expect("password alphabet is ASCII"))
}

/// Draw a uniform random u64 from the OS RNG.
fn random_u64() -> Result<u64, LychiError> {
    let mut buf = [0u8; 8];
    getrandom::fill(&mut buf)
        .map_err(|e| LychiError::ExecutionFailed(format!("Random generation failed: {e}")))?;
    Ok(u64::from_le_bytes(buf))
}

/// Random integer in `[min, max]` inclusive. Rejection sampling keeps the
/// distribution uniform (avoids modulo bias). `min`/`max` may be given in any
/// order; equal bounds return that value.
fn random_int(min: i64, max: i64) -> Result<i64, LychiError> {
    let (lo, hi) = if min <= max { (min, max) } else { (max, min) };
    let span = (hi as i128 - lo as i128) as u128 + 1; // inclusive range size
    if span == 0 || span > u64::MAX as u128 {
        return Err(LychiError::ExecutionFailed("Range too large".to_string()));
    }
    let span = span as u64;
    // Reject the biased tail so every value is equally likely.
    let limit = u64::MAX - (u64::MAX % span);
    loop {
        let r = random_u64()?;
        if r < limit || limit == 0 {
            return Ok(lo + (r % span) as i64);
        }
    }
}

/// Parse the `random` subcommand args into (min, max). Accepts:
///   ""            → 0..=100
///   "<max>"       → 0..=max
///   "<min> <max>" → min..=max
fn parse_random_range(rest: &str) -> Result<(i64, i64), LychiError> {
    let parts: Vec<&str> = rest.split_whitespace().collect();
    let parse = |s: &str| {
        s.parse::<i64>()
            .map_err(|_| LychiError::ExecutionFailed(format!("Invalid number: {s}")))
    };
    match parts.as_slice() {
        [] => Ok((0, 100)),
        [max] => Ok((0, parse(max)?)),
        [min, max] => Ok((parse(min)?, parse(max)?)),
        _ => Err(LychiError::ExecutionFailed(
            "Usage: generate random [min] <max>".to_string(),
        )),
    }
}

/// Generate a URL-safe base64 token.
fn generate_token(length: usize) -> Result<String, LychiError> {
    if !(8..=256).contains(&length) {
        return Err(LychiError::ExecutionFailed(
            "Token length must be between 8 and 256".to_string(),
        ));
    }

    // Each random byte maps to one output char
    let mut buf = vec![0u8; length];
    getrandom::fill(&mut buf)
        .map_err(|e| LychiError::ExecutionFailed(format!("Random generation failed: {e}")))?;

    // URL-safe base64 without padding
    const CHARSET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let encoded: String = buf
        .iter()
        .flat_map(|&b| {
            // Each byte gives us slightly more than one base64 char,
            // but simplest to just map each byte to a charset char
            std::iter::once(CHARSET[b as usize % CHARSET.len()] as char)
        })
        .take(length)
        .collect();

    Ok(encoded)
}

#[async_trait]
impl ActionHandler for GenerateHandler {
    fn triggers(&self) -> &'static [crate::action_registry::Trigger] {
        use crate::action_registry::{ArgTransform, Trigger};
        static TRIGGERS: &[Trigger] = &[
            Trigger::keywords(&["generate", "gen"]),
            Trigger::new(&["password"], ArgTransform::Prepend("password")),
            Trigger::new(&["uuid"], ArgTransform::Fixed("uuid")),
            Trigger::new(&["token"], ArgTransform::Prepend("token")),
            Trigger::new(&["random", "rand"], ArgTransform::Prepend("random")),
        ];
        TRIGGERS
    }

    fn id(&self) -> &str {
        "generate"
    }

    fn execution_mode(&self) -> crate::action_registry::ExecutionMode {
        crate::action_registry::ExecutionMode::ReplacePrevious
    }

    fn description(&self) -> &str {
        "Generate passwords, UUIDs, tokens, and random numbers"
    }
    fn category(&self) -> CommandCategory {
        CommandCategory::Utilities
    }

    async fn execute(&self, _ctx: &ExecContext, args: &str) -> Result<ActionResult, LychiError> {
        let args = args.trim();
        let (subcmd, rest) = match args.split_once(' ') {
            Some((s, r)) => (s, r.trim()),
            None => (args, ""),
        };

        match subcmd.to_lowercase().as_str() {
            "password" | "pw" | "pass" => {
                let length: usize = if rest.is_empty() {
                    16
                } else {
                    rest.parse().map_err(|_| {
                        LychiError::ExecutionFailed(format!("Invalid length: {rest}"))
                    })?
                };
                let pw = generate_password(length)?;
                let _ = write_to_clipboard(&pw);
                Ok(ActionResult::ok(
                    format!("{pw}\n\n📋 Copied to clipboard ({length} chars)"),
                    OutputType::Terminal,
                ))
            }
            "uuid" => {
                let id = uuid::Uuid::new_v4().to_string();
                let _ = write_to_clipboard(&id);
                Ok(ActionResult::ok(
                    format!("{id}\n\n📋 Copied to clipboard"),
                    OutputType::Terminal,
                ))
            }
            "token" | "tok" => {
                let length: usize = if rest.is_empty() {
                    32
                } else {
                    rest.parse().map_err(|_| {
                        LychiError::ExecutionFailed(format!("Invalid length: {rest}"))
                    })?
                };
                let tok = generate_token(length)?;
                let _ = write_to_clipboard(&tok);
                Ok(ActionResult::ok(
                    format!("{tok}\n\n📋 Copied to clipboard ({length} chars)"),
                    OutputType::Terminal,
                ))
            }
            "random" | "rand" | "number" | "num" => {
                let (min, max) = parse_random_range(rest)?;
                let n = random_int(min, max)?;
                let _ = write_to_clipboard(&n.to_string());
                Ok(ActionResult::ok(
                    format!("{n}\n\n📋 Copied to clipboard (random {min}–{max})"),
                    OutputType::Terminal,
                ))
            }
            "" => Ok(ActionResult::ok(
                "Usage:\n  generate password [length]  — random password (default 16)\n  generate uuid               — UUIDv4\n  generate token [length]     — URL-safe token (default 32)\n  generate random [min] <max> — random integer (default 0–100)",
                OutputType::Text,
            )),
            other => Ok(ActionResult::err(format!(
                "Unknown subcommand '{other}'. Use: password, uuid, token, random"
            ))),
        }
    }

    async fn completions(&self, partial: &str) -> Vec<CompletionItem> {
        let partial_lower = partial.trim().to_lowercase();

        let hints = [
            (
                "password",
                "password [length]",
                "Random password (default 16 chars)",
            ),
            ("uuid", "uuid", "Generate UUIDv4"),
            (
                "token",
                "token [length]",
                "URL-safe base64 token (default 32)",
            ),
            (
                "random",
                "random [min] <max>",
                "Random integer (default 0–100)",
            ),
        ];

        hints
            .iter()
            .filter(|(key, _, _)| partial_lower.is_empty() || key.starts_with(&partial_lower))
            .enumerate()
            .map(|(i, (key, label, desc))| CompletionItem {
                label: label.to_string(),
                icon_path: None,
                score: (900 - i as u16).max(1),
                description: Some(desc.to_string()),
                reason: None,
                thumb_b64: None,
                // Strip the "[length]" placeholder — run the bare subcommand
                // so it uses the default length.
                run: Some(format!("generate {key}")),
                ..Default::default()
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_default_length() {
        let pw = generate_password(16).unwrap();
        assert_eq!(pw.len(), 16);
    }

    #[test]
    fn password_custom_length() {
        let pw = generate_password(32).unwrap();
        assert_eq!(pw.len(), 32);
    }

    #[test]
    fn password_has_all_classes() {
        // Generate several passwords and check all have each class
        for _ in 0..10 {
            let pw = generate_password(16).unwrap();
            assert!(
                pw.chars().any(|c| c.is_ascii_uppercase()),
                "missing upper: {pw}"
            );
            assert!(
                pw.chars().any(|c| c.is_ascii_lowercase()),
                "missing lower: {pw}"
            );
            assert!(
                pw.chars().any(|c| c.is_ascii_digit()),
                "missing digit: {pw}"
            );
            assert!(
                pw.chars().any(|c| !c.is_alphanumeric()),
                "missing symbol: {pw}"
            );
        }
    }

    #[test]
    fn password_rejects_too_short() {
        assert!(generate_password(7).is_err());
    }

    #[test]
    fn password_rejects_too_long() {
        assert!(generate_password(129).is_err());
    }

    #[test]
    fn uuid_format() {
        let id = uuid::Uuid::new_v4().to_string();
        assert!(
            id.len() == 36 && id.chars().filter(|c| *c == '-').count() == 4,
            "bad UUID: {id}"
        );
    }

    #[test]
    fn token_default_length() {
        let tok = generate_token(32).unwrap();
        assert_eq!(tok.len(), 32);
    }

    #[test]
    fn token_custom_length() {
        let tok = generate_token(64).unwrap();
        assert_eq!(tok.len(), 64);
    }

    #[test]
    fn token_is_url_safe() {
        let tok = generate_token(100).unwrap();
        assert!(
            tok.chars()
                .all(|c| c.is_alphanumeric() || c == '-' || c == '_'),
            "non-url-safe char in: {tok}"
        );
    }

    #[test]
    fn token_rejects_too_short() {
        assert!(generate_token(7).is_err());
    }

    #[test]
    fn random_int_stays_in_range() {
        for _ in 0..200 {
            let n = random_int(1, 6).unwrap();
            assert!((1..=6).contains(&n), "out of range: {n}");
        }
    }

    #[test]
    fn random_int_equal_bounds() {
        assert_eq!(random_int(42, 42).unwrap(), 42);
    }

    #[test]
    fn random_int_handles_reversed_bounds() {
        for _ in 0..50 {
            let n = random_int(100, 1).unwrap();
            assert!((1..=100).contains(&n), "out of range: {n}");
        }
    }

    #[test]
    fn random_int_negative_range() {
        for _ in 0..50 {
            let n = random_int(-10, -5).unwrap();
            assert!((-10..=-5).contains(&n), "out of range: {n}");
        }
    }

    #[test]
    fn parse_random_range_forms() {
        assert_eq!(parse_random_range("").unwrap(), (0, 100));
        assert_eq!(parse_random_range("50").unwrap(), (0, 50));
        assert_eq!(parse_random_range("10 20").unwrap(), (10, 20));
        assert!(parse_random_range("a b").is_err());
        assert!(parse_random_range("1 2 3").is_err());
    }
}

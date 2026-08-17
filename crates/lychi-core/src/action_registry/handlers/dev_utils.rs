//! Developer utilities — small, offline, text-in/text-out conversions devs
//! reach for constantly: base64, hashing, URL encode/decode, unix epoch, JSON
//! formatting, and text-case transforms.
//!
//! Each is a distinct verb under one handler:
//!   - `base64 <text>`          → base64-encode
//!   - `base64 -d <b64>`        → base64-decode
//!   - `hash <text>`            → sha256 (default)
//!   - `hash md5|sha256 <text>`
//!   - `urlencode <text>` / `urldecode <text>`
//!   - `epoch`                  → current unix time
//!   - `epoch <secs>`           → that unix time as a UTC datetime
//!   - `json <text>`            → pretty-print JSON
//!   - `json -m <text>`         → minify JSON
//!   - `upper`/`lower`/`title`  → change case
//!   - `slug <text>`            → url-safe slug ("My Post" → "my-post")
//!   - `reverse <text>`         → reverse characters
//!   - `count <text>`           → chars / words / lines
//!
//! Fully local, deterministic, no network. Output is shown as text and copyable.

use async_trait::async_trait;
use base64::Engine;

use crate::action_registry::grammar::{ArgKind, Grammar, Operand, ToolGroup, Verb};
use crate::action_registry::{
    ActionHandler, ActionResult, CommandCategory, CompletionItem, ExecContext, OutputType,
};
use crate::error::LychiError;

pub struct DevUtilsHandler;

impl DevUtilsHandler {
    pub fn new() -> Self {
        Self
    }
}

impl Default for DevUtilsHandler {
    fn default() -> Self {
        Self::new()
    }
}

fn text_result(output: String) -> ActionResult {
    ActionResult::ok(output, OutputType::Text)
}

/// Every dev-util verb `run` dispatches — the single list the triggers, the
/// live-preview gate and the tool schema all read, so none of them can drift
/// from the parser.
const DEV_UTIL_VERBS: &[&str] = &[
    "base64",
    "hash",
    "urlencode",
    "urldecode",
    "epoch",
    "json",
    "upper",
    "lower",
    "title",
    "slug",
    "reverse",
    "count",
];

/// The input-text operand shared by the text-in/text-out verbs. One const so
/// the merged group-schema field carries one coherent description across every
/// verb that reads it.
const TEXT: Operand = Operand {
    name: "text",
    desc: "The input text: the string to encode/hash/transform, the base64 or \
           URL-encoded value to decode, or the raw JSON to format.",
    required: true,
    kind: ArgKind::Text,
    prefix: None,
};

/// `devutil`'s argument surface: one verb per [`DEV_UTIL_VERBS`] entry (a
/// drift test pins the two lists together), with the per-verb modifiers
/// (`decode`, `minify`, `algorithm`) as typed operands whose flat renderings
/// are the flag strings the parser already reads (`-d`, `-m`, `md5`). The
/// JSON Schema and the structured→flat adapter both derive from this. All
/// verbs are pure, local, deterministic text conversions — nothing mutates.
const DEVUTIL_GRAMMAR: Grammar = Grammar {
    verbs: &[
        Verb {
            name: "base64",
            desc: "Base64-encode text, or decode a base64 string back to text \
                   (set decode).",
            mutates: false,
            operands: &[
                Operand {
                    name: "decode",
                    desc: "true to DECODE `text` from base64 instead of encoding \
                           it. Default false (encode).",
                    required: false,
                    kind: ArgKind::Bool { flag: "-d" },
                    prefix: None,
                },
                TEXT,
            ],
        },
        Verb {
            name: "hash",
            desc: "Compute a hex digest of the text — sha256 by default, or md5.",
            mutates: false,
            operands: &[
                Operand {
                    name: "algorithm",
                    desc: "The digest algorithm. Default sha256 when omitted.",
                    required: false,
                    kind: ArgKind::Choice(&["sha256", "md5"]),
                    prefix: None,
                },
                TEXT,
            ],
        },
        Verb {
            name: "urlencode",
            desc: "Percent-encode text for safe inclusion in a URL.",
            mutates: false,
            operands: &[TEXT],
        },
        Verb {
            name: "urldecode",
            desc: "Decode a percent-encoded (URL-encoded) string back to plain \
                   text.",
            mutates: false,
            operands: &[TEXT],
        },
        Verb {
            name: "epoch",
            desc: "Convert unix seconds to a UTC datetime, or get the current \
                   unix time when no seconds are given.",
            mutates: false,
            operands: &[Operand {
                name: "seconds",
                desc: "Unix seconds to render as a UTC datetime (e.g. \
                       1700000000). Omit for the current unix time.",
                required: false,
                kind: ArgKind::Int,
                prefix: None,
            }],
        },
        Verb {
            name: "json",
            desc: "Pretty-print JSON with 2-space indentation, or minify it to \
                   one line (set minify).",
            mutates: false,
            operands: &[
                Operand {
                    name: "minify",
                    desc: "true to minify to one line instead of pretty-printing. \
                           Default false (pretty-print).",
                    required: false,
                    kind: ArgKind::Bool { flag: "-m" },
                    prefix: None,
                },
                TEXT,
            ],
        },
        Verb {
            name: "upper",
            desc: "Uppercase the text.",
            mutates: false,
            operands: &[TEXT],
        },
        Verb {
            name: "lower",
            desc: "Lowercase the text.",
            mutates: false,
            operands: &[TEXT],
        },
        Verb {
            name: "title",
            desc: "Title-Case Each Word of the text.",
            mutates: false,
            operands: &[TEXT],
        },
        Verb {
            name: "slug",
            desc: "Turn text into a url/filename-safe slug (\"My Post\" → \
                   \"my-post\").",
            mutates: false,
            operands: &[TEXT],
        },
        Verb {
            name: "reverse",
            desc: "Reverse the characters of the text.",
            mutates: false,
            operands: &[TEXT],
        },
        Verb {
            name: "count",
            desc: "Count characters, words, and lines in the text.",
            mutates: false,
            operands: &[TEXT],
        },
    ],
};

/// Normalize the tool's `args` to the flat `"<verb> [flags] <text>"` string
/// `execute` already parses (`base64 -d <b64>`, `json -m <text>`,
/// `hash md5 <text>`, …). A constrained model sends the structured JSON; a
/// human or legacy/flat caller sends the string directly and passes through
/// unchanged (malformed/unknown-action JSON included — `run` answers with its
/// usual usage message). Keeps `execute` on `&str`. The walk itself lives in
/// [`Grammar::flatten_json`] — the ONE structured→flat decider.
fn devutil_args_to_flat(args: &str) -> String {
    DEVUTIL_GRAMMAR
        .flatten_json(args)
        .unwrap_or_else(|| args.trim().to_string())
}

/// Run one dev-util verb. Pure (no I/O) so it's fully unit-testable.
fn run(verb: &str, args: &str) -> Result<String, String> {
    match verb {
        "base64" => {
            // `-d`/`--decode` flag decodes; otherwise encode.
            if let Some(rest) = args
                .strip_prefix("-d ")
                .or_else(|| args.strip_prefix("--decode "))
            {
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(rest.trim())
                    .map_err(|e| format!("Invalid base64: {e}"))?;
                String::from_utf8(bytes)
                    .map_err(|_| "Decoded bytes are not valid UTF-8".to_string())
            } else if args.trim().is_empty() {
                Err("Usage: base64 <text>  (or  base64 -d <base64>)".to_string())
            } else {
                Ok(base64::engine::general_purpose::STANDARD.encode(args))
            }
        }
        "hash" => {
            // `hash <text>` → sha256; `hash <algo> <text>` for md5/sha256.
            let (algo, text) = match args.split_once(char::is_whitespace) {
                Some((a, rest)) if matches!(a, "md5" | "sha256") => (a, rest.trim()),
                _ => ("sha256", args.trim()),
            };
            if text.is_empty() {
                return Err("Usage: hash [md5|sha256] <text>".to_string());
            }
            Ok(match algo {
                "md5" => {
                    use md5::Digest;
                    hex::encode(md5::Md5::digest(text.as_bytes()))
                }
                _ => {
                    use sha2::Digest;
                    hex::encode(sha2::Sha256::digest(text.as_bytes()))
                }
            })
        }
        "urlencode" => {
            if args.trim().is_empty() {
                return Err("Usage: urlencode <text>".to_string());
            }
            Ok(urlencoding::encode(args).into_owned())
        }
        "urldecode" => {
            if args.trim().is_empty() {
                return Err("Usage: urldecode <text>".to_string());
            }
            urlencoding::decode(args.trim())
                .map(|s| s.into_owned())
                .map_err(|e| format!("Invalid URL encoding: {e}"))
        }
        "epoch" => {
            if args.trim().is_empty() {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                Ok(now.to_string())
            } else {
                let secs: i64 = args
                    .trim()
                    .parse()
                    .map_err(|_| "Usage: epoch [<unix-seconds>]".to_string())?;
                Ok(format_epoch(secs))
            }
        }
        "json" => {
            // `json -m <text>` minifies; otherwise pretty-print (2-space indent).
            let (minify, text) = match args
                .strip_prefix("-m ")
                .or_else(|| args.strip_prefix("--minify "))
            {
                Some(rest) => (true, rest.trim()),
                None => (false, args.trim()),
            };
            if text.is_empty() {
                return Err("Usage: json <text>  (or  json -m <text> to minify)".to_string());
            }
            let value: serde_json::Value =
                serde_json::from_str(text).map_err(|e| format!("Invalid JSON: {e}"))?;
            if minify {
                serde_json::to_string(&value).map_err(|e| e.to_string())
            } else {
                serde_json::to_string_pretty(&value).map_err(|e| e.to_string())
            }
        }
        "upper" => require_text(args).map(|t| t.to_uppercase()),
        "lower" => require_text(args).map(|t| t.to_lowercase()),
        "title" => require_text(args).map(title_case),
        "slug" => require_text(args).map(slugify),
        "reverse" => require_text(args).map(|t| t.chars().rev().collect()),
        "count" => require_text(args).map(|t| {
            let chars = t.chars().count();
            let words = t.split_whitespace().count();
            let lines = t.lines().count().max(1);
            format!("{chars} chars, {words} words, {lines} lines")
        }),
        _ => Err(format!("Unknown dev util: {verb}")),
    }
}

/// Return the trimmed args or a usage error if empty. Shared by the text verbs.
fn require_text(args: &str) -> Result<&str, String> {
    let t = args.trim();
    if t.is_empty() {
        Err("Usage: <verb> <text>".to_string())
    } else {
        Ok(t)
    }
}

/// Title-case each whitespace-separated word (first letter upper, rest lower).
fn title_case(text: &str) -> String {
    text.split_whitespace()
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => {
                    first.to_uppercase().collect::<String>() + &chars.as_str().to_lowercase()
                }
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// URL/filename-safe slug: lowercase, alphanumerics kept, runs of anything else
/// collapsed to a single hyphen, no leading/trailing hyphens.
fn slugify(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut prev_dash = true; // suppress leading hyphen
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// Format a unix timestamp as a UTC datetime without pulling in chrono — a
/// civil-time conversion (days since epoch → Y/M/D) plus H:M:S.
fn format_epoch(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let (y, mo, d) = civil_from_days(days);
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{m:02}:{s:02} UTC")
}

/// Howard Hinnant's `civil_from_days`: days-since-1970 → (year, month, day).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[async_trait]
impl ActionHandler for DevUtilsHandler {
    fn triggers(&self) -> &'static [crate::action_registry::Trigger] {
        use crate::action_registry::{ArgTransform, Trigger};
        static TRIGGERS: &[Trigger] = &[Trigger::new(DEV_UTIL_VERBS, ArgTransform::PrependKeyword)];
        TRIGGERS
    }

    fn id(&self) -> &str {
        "devutil"
    }

    fn description(&self) -> &str {
        "Developer utilities: base64, hash, urlencode/decode, epoch, json, text-case"
    }
    fn usage(&self) -> &str {
        "Prepend the verb to the text. Verbs: 'base64 <text>' / 'base64 -d <b64>', 'hash [md5|sha256] <text>', 'urlencode <text>' / 'urldecode <text>', 'epoch [<unix-seconds>]', 'json <text>' (pretty-print) / 'json -m <text>' (minify), 'upper/lower/title <text>', 'slug <text>', 'reverse <text>', 'count <text>'. Use for encode/decode, format json, slugify, etc."
    }
    fn category(&self) -> CommandCategory {
        CommandCategory::Developer
    }
    fn grammar(&self) -> Option<Grammar> {
        Some(DEVUTIL_GRAMMAR)
    }
    fn tool_group(&self) -> ToolGroup {
        ToolGroup::Dev
    }

    async fn completions(&self, partial: &str) -> Vec<CompletionItem> {
        // Live preview: run the verb against what's typed so far and show the
        // result inline as the user types (like calc).
        let trimmed = partial.trim();
        let Some((verb, args)) = trimmed.split_once(char::is_whitespace) else {
            return Vec::new();
        };
        if !DEV_UTIL_VERBS.contains(&verb) {
            return Vec::new();
        }
        match run(verb, args) {
            Ok(out) if !out.is_empty() => vec![
                CompletionItem::new(format!("= {out}"), Some("__none__".into()), 100)
                    .with_run(trimmed.to_string())
                    .with_description(verb.to_string()),
            ],
            _ => Vec::new(),
        }
    }

    async fn execute(&self, _ctx: &ExecContext, args: &str) -> Result<ActionResult, LychiError> {
        // Called with the full "verb args" (patterns routes each verb here). A
        // constrained model sends `{"action":..,"text":..}`; flatten it (and a
        // plain-string caller passes through) first.
        let flat = devutil_args_to_flat(args);
        let trimmed = flat.trim();
        let (verb, rest) = trimmed
            .split_once(char::is_whitespace)
            .unwrap_or((trimmed, ""));
        match run(verb, rest) {
            Ok(out) => Ok(text_result(out)),
            Err(e) => Ok(ActionResult::err(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_roundtrip() {
        assert_eq!(run("base64", "hello").unwrap(), "aGVsbG8=");
        assert_eq!(run("base64", "-d aGVsbG8=").unwrap(), "hello");
    }

    #[test]
    fn base64_bad_decode_errors() {
        assert!(run("base64", "-d !!!not-base64!!!").is_err());
    }

    #[test]
    fn hash_defaults_to_sha256() {
        // sha256("abc") known vector.
        assert_eq!(
            run("hash", "abc").unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn hash_md5() {
        // md5("abc") known vector.
        assert_eq!(
            run("hash", "md5 abc").unwrap(),
            "900150983cd24fb0d6963f7d28e17f72"
        );
    }

    #[test]
    fn url_encode_decode() {
        assert_eq!(run("urlencode", "a b&c").unwrap(), "a%20b%26c");
        assert_eq!(run("urldecode", "a%20b%26c").unwrap(), "a b&c");
    }

    #[test]
    fn epoch_formats_known_timestamp() {
        // 1_700_000_000 = 2023-11-14 22:13:20 UTC.
        assert_eq!(
            run("epoch", "1700000000").unwrap(),
            "2023-11-14 22:13:20 UTC"
        );
    }

    #[test]
    fn epoch_zero_is_unix_start() {
        assert_eq!(run("epoch", "0").unwrap(), "1970-01-01 00:00:00 UTC");
    }

    #[test]
    fn usage_errors_are_friendly() {
        assert!(run("base64", "").is_err());
        assert!(run("hash", "").is_err());
        assert!(run("urlencode", "").is_err());
    }

    #[test]
    fn json_pretty_prints() {
        let out = run("json", r#"{"a":1,"b":[2,3]}"#).unwrap();
        assert!(
            out.contains("\"a\": 1"),
            "expected pretty output, got: {out}"
        );
        assert!(out.contains('\n'), "pretty output should be multiline");
    }

    #[test]
    fn json_minifies_with_flag() {
        let out = run("json", "-m {\n  \"a\": 1\n}").unwrap();
        assert_eq!(out, r#"{"a":1}"#);
    }

    #[test]
    fn json_rejects_invalid() {
        assert!(run("json", "{not json}").is_err());
        assert!(run("json", "").is_err());
    }

    #[test]
    fn case_transforms() {
        assert_eq!(run("upper", "Hello World").unwrap(), "HELLO WORLD");
        assert_eq!(run("lower", "Hello World").unwrap(), "hello world");
        assert_eq!(run("title", "hello wORLD").unwrap(), "Hello World");
    }

    #[test]
    fn slug_basic() {
        assert_eq!(run("slug", "My First Post!").unwrap(), "my-first-post");
        assert_eq!(run("slug", "  Hello -- World  ").unwrap(), "hello-world");
        assert_eq!(run("slug", "café_déjà").unwrap(), "caf-d-j");
    }

    #[test]
    fn reverse_and_count() {
        assert_eq!(run("reverse", "abc").unwrap(), "cba");
        assert_eq!(
            run("count", "one two three").unwrap(),
            "13 chars, 3 words, 1 lines"
        );
    }

    #[test]
    fn text_verbs_require_input() {
        assert!(run("upper", "").is_err());
        assert!(run("slug", "   ").is_err());
    }

    #[test]
    fn devutil_args_flatten_from_structured_json() {
        // A constrained model sends the typed object; the boolean/enum
        // modifiers become the flag strings `run` already parses.
        assert_eq!(
            devutil_args_to_flat(r#"{"action":"base64","text":"hello"}"#),
            "base64 hello"
        );
        assert_eq!(
            devutil_args_to_flat(r#"{"action":"base64","text":"aGVsbG8=","decode":true}"#),
            "base64 -d aGVsbG8="
        );
        assert_eq!(
            devutil_args_to_flat(r#"{"action":"json","text":"{\"a\":1}","minify":true}"#),
            r#"json -m {"a":1}"#
        );
        assert_eq!(
            devutil_args_to_flat(r#"{"action":"hash","text":"abc","algorithm":"md5"}"#),
            "hash md5 abc"
        );
        // Default algorithm is left to the parser (sha256), not restated here.
        assert_eq!(
            devutil_args_to_flat(r#"{"action":"hash","text":"abc"}"#),
            "hash abc"
        );
        // Bare epoch (no text) is the one verb valid without input; `seconds`
        // renders as its decimal form when present.
        assert_eq!(devutil_args_to_flat(r#"{"action":"epoch"}"#), "epoch");
        assert_eq!(
            devutil_args_to_flat(r#"{"action":"epoch","seconds":1700000000}"#),
            "epoch 1700000000"
        );
        // A modifier without text renders its flag verbatim — `text` is a
        // REQUIRED operand, and that requirement is enforced by the group
        // dispatcher (`resolve_group_call`), not re-implemented here: the
        // adapter is a pure rendering of the grammar's ONE flatten decider.
        assert_eq!(
            devutil_args_to_flat(r#"{"action":"base64","decode":true}"#),
            "base64 -d"
        );
        // A plain-string caller (human, legacy) passes straight through.
        assert_eq!(
            devutil_args_to_flat("base64 -d aGVsbG8="),
            "base64 -d aGVsbG8="
        );
        assert_eq!(devutil_args_to_flat("slug My Post"), "slug My Post");
        // Malformed JSON → raw fallback.
        assert_eq!(devutil_args_to_flat("{not json"), "{not json");
    }

    #[test]
    fn devutil_grammar_verbs_match_the_real_verbs() {
        // The grammar's verb list must be exactly DEV_UTIL_VERBS — the same
        // list the triggers and the preview gate read — so the derived schema
        // constrains the model to verbs `run` actually handles, in the same
        // stable order.
        let names: Vec<&str> = DEVUTIL_GRAMMAR.verbs.iter().map(|v| v.name).collect();
        assert_eq!(names, DEV_UTIL_VERBS);
        // And the derived schema's enum is that list verbatim.
        let schema = DEVUTIL_GRAMMAR.handler_schema();
        let en = schema["properties"]["action"]["enum"].as_array().unwrap();
        let schema_names: Vec<&str> = en.iter().filter_map(|e| e.as_str()).collect();
        assert_eq!(schema_names, DEV_UTIL_VERBS);
    }

    #[test]
    fn every_grammar_verbs_flat_rendering_is_accepted_by_the_parser() {
        // Drift guard: for each verb, render a structured call through the
        // grammar and feed the result to the real parser — it must dispatch
        // (never "Unknown dev util") and, where the input is well-formed,
        // succeed. Uses each verb's own operands so modifiers render too.
        for verb in DEVUTIL_GRAMMAR.verbs {
            let json = match verb.name {
                "base64" => r#"{"action":"base64","text":"aGk=","decode":true}"#.to_string(),
                "hash" => r#"{"action":"hash","text":"abc","algorithm":"md5"}"#.to_string(),
                "epoch" => r#"{"action":"epoch","seconds":1700000000}"#.to_string(),
                "json" => r#"{"action":"json","text":"{\"a\":1}","minify":true}"#.to_string(),
                name => format!(r#"{{"action":"{name}","text":"abc def"}}"#),
            };
            let flat = devutil_args_to_flat(&json);
            let (verb_tok, rest) = flat
                .split_once(char::is_whitespace)
                .unwrap_or((flat.as_str(), ""));
            assert_eq!(verb_tok, verb.name, "flat form must lead with the verb");
            let out = run(verb_tok, rest);
            assert!(
                out.is_ok(),
                "parser rejected the grammar's rendering for {}: {flat:?} → {out:?}",
                verb.name
            );
        }
    }
}

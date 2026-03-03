//! Smart input router — deterministic pre-filter for intent routing.
//!
//! Routes user input to the appropriate handler based on:
//! 1. Explicit prefixes (`web `, `yt `, `run `, `open `, `calc `, etc.)
//! 2. Trigger characters (`?` web search, `=` calc, `>` shell)
//! 3. Pattern detection (file paths, URLs, math/conversion expressions)
//! 4. No match: `PatternResult::NoMatch` — IntentResolver uses AI, falls back to open→web

/// Known handler prefixes — single source of truth for all keyword recognition.
/// Used by: pattern routing, typo correction, frontend completion handling.
pub const KNOWN_PREFIXES: &[&str] = &[
    // Explicit handler prefixes
    "ask",
    "bm",
    "bookmark",
    "browse",
    "clip",
    "clipboard",
    "ctx",
    "close",
    "emoji",
    "focus",
    "kill",
    "open",
    "sym",
    "unicode",
    "web",
    "yt",
    "run",
    "calc",
    "calculator",
    "file",
    "url",
    "media",
    "project",
    "quit",
    "system",
    "note",
    "notes",
    "todo",
    "todos",
    "snip",
    "snippet",
    "snippets",
    "weather",
    "sysinfo",
    "ip",
    "cpu",
    "mem",
    "disk",
    "temp",
    "gpu",
    "battery",
    "net",
    "audio",
    "display",
    "os",
    "speedtest",
    "time",
    "tz",
    "clock",
    "alias",
    "aliases",
    "timer",
    "stopwatch",
    "reminder",
    "remind",
    // System power commands — explicit single-word triggers
    "shutdown",
    "poweroff",
    "reboot",
    "restart",
    "hibernate",
    "lock",
    "suspend",
    "sleep",
    "logout",
    "signout",
    "mute",
    "unmute",
    "volume",
    "brightness",
    "bluetooth",
    "spotify",
    "pomodoro",
    "recent",
    // Sysinfo keywords
    "memory",
    "temperature",
];

/// Common TLDs for URL detection.
const TLDS: &[&str] = &[
    ".com", ".org", ".net", ".io", ".dev", ".app", ".co", ".me", ".info", ".xyz", ".edu", ".gov",
    ".mil", ".int", ".eu", ".uk", ".de", ".fr", ".jp", ".cn", ".au", ".ca", ".br", ".in", ".ru",
    ".ly", ".ai", ".gg", ".tv", ".cc",
];

/// A deterministically matched route from patterns.rs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    /// Handler to dispatch to (e.g. "web", "calc", "run", "file", "url")
    pub handler: &'static str,
    /// Arguments to pass to the handler
    pub args: String,
    /// Whether this route was explicitly triggered (prefix or trigger char)
    pub explicit: bool,
}

/// Result of deterministic pattern matching.
///
/// `Match` means patterns.rs is confident — dispatch immediately, no AI needed.
/// `NoMatch` means no structural pattern found — IntentResolver should try AI,
/// then fall back to AppIndex → web search.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatternResult {
    Match(Route),
    NoMatch { input: String },
}

impl PatternResult {
    /// Returns true if this is a deterministic match.
    pub fn is_match(&self) -> bool {
        matches!(self, PatternResult::Match(_))
    }

    /// Unwrap the inner Route, panicking if this is NoMatch. For tests only.
    #[cfg(test)]
    pub fn unwrap(self) -> Route {
        match self {
            PatternResult::Match(r) => r,
            PatternResult::NoMatch { input } => {
                panic!("called unwrap() on PatternResult::NoMatch {{ input: {input:?} }}")
            }
        }
    }
}

/// Check if a word is a known handler prefix (used by typo_suggest to skip exact matches).
pub fn is_known_prefix(word: &str) -> bool {
    KNOWN_PREFIXES.iter().any(|p| p.eq_ignore_ascii_case(word))
}

/// Route raw user input to the appropriate handler.
pub fn route(raw: &str) -> PatternResult {
    route_inner(raw, true)
}

fn route_inner(raw: &str, check_aliases: bool) -> PatternResult {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return PatternResult::NoMatch {
            input: String::new(),
        };
    }

    // 1. Explicit prefix — first word matches a known handler
    if let Some(r) = try_explicit_prefix(trimmed) {
        return PatternResult::Match(r);
    }

    // 2. Trigger characters & shorthand colon-prefixes
    //    Multi-char prefixes checked first (longest match wins), then single-char.
    static COLON_TRIGGERS: &[(&str, &str)] = &[
        ("bm:", "bm"),
        ("cl:", "clip"),
        ("sym:", "sym"),
        ("sys:", "system"),
        ("si:", "sysinfo"),
        ("yt:", "yt"),
        ("e:", "emoji"),
        ("u:", "unicode"),
        ("w:", "web"),
        ("r:", "run"),
        ("c:", "calc"),
        ("f:", "file"),
        ("o:", "open"),
        ("n:", "note"),
        ("t:", "todo"),
        ("m:", "media"),
        ("p:", "project"),
        ("tz:", "time"),
        ("al:", "alias"),
        ("sn:", "snip"),
        ("tm:", "timer"),
        ("rm:", "reminder"),
        ("a:", "ask"),
    ];
    for &(prefix, handler) in COLON_TRIGGERS {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            return PatternResult::Match(Route {
                handler,
                args: rest.trim().to_string(),
                explicit: true,
            });
        }
    }
    if let Some(query) = trimmed.strip_prefix('?') {
        return PatternResult::Match(Route {
            handler: "web",
            args: query.trim().to_string(),
            explicit: true,
        });
    }
    if let Some(expr) = trimmed.strip_prefix('=') {
        return PatternResult::Match(Route {
            handler: "calc",
            args: expr.trim().to_string(),
            explicit: true,
        });
    }
    if let Some(cmd) = trimmed.strip_prefix('>') {
        return PatternResult::Match(Route {
            handler: "run",
            args: cmd.trim().to_string(),
            explicit: true,
        });
    }

    // 3. File path
    if trimmed.starts_with('/') || trimmed.starts_with("~/") || trimmed.starts_with("./") {
        return PatternResult::Match(Route {
            handler: "file",
            args: trimmed.to_string(),
            explicit: false,
        });
    }

    // 4. URL
    if looks_like_url(trimmed) {
        return PatternResult::Match(Route {
            handler: "url",
            args: trimmed.to_string(),
            explicit: false,
        });
    }

    // 5. Math expression (digits + operators only, no letters)
    if is_math_expression(trimmed) {
        return PatternResult::Match(Route {
            handler: "calc",
            args: trimmed.to_string(),
            explicit: false,
        });
    }

    // 5b. Unit/currency conversion (e.g. "5 kg to lb", "100 usd to eur")
    if crate::action_registry::handlers::calc::is_conversion_expression(trimmed) {
        return PatternResult::Match(Route {
            handler: "calc",
            args: trimmed.to_string(),
            explicit: false,
        });
    }

    // 5c. Structured power phrases — unambiguous, handle before AI fallback
    let lower = trimmed.to_lowercase();
    if lower.starts_with("shutdown in ") || lower.starts_with("shut down in ") {
        return PatternResult::Match(Route {
            handler: "system",
            args: lower,
            explicit: false,
        });
    }
    if lower == "cancel shutdown" || lower == "shutdown cancel" {
        return PatternResult::Match(Route {
            handler: "system",
            args: "cancel shutdown".to_string(),
            explicit: false,
        });
    }

    // 6. Alias resolution — check if first word matches a stored alias.
    //      Only on the first pass (check_aliases=true) to prevent infinite recursion
    //      if an alias expands to another alias name.
    if check_aliases {
        let first_word = trimmed.split_whitespace().next().unwrap_or("");
        if let Some(expanded) = crate::aliases::store::lookup(&first_word.to_lowercase()) {
            let extra = trimmed.split_once(' ').map(|(_, rest)| rest).unwrap_or("");
            let full = if extra.is_empty() {
                expanded
            } else {
                format!("{expanded} {extra}")
            };
            return route_inner(&full, false);
        }
    }

    // 7. Home directory entry (e.g. "Downloads", "Documents", "report.pdf")
    if !trimmed.contains(' ')
        && let Some(home) = dirs::home_dir()
    {
        let candidate = home.join(trimmed);
        if candidate.exists() {
            return PatternResult::Match(Route {
                handler: "file",
                args: format!("~/{trimmed}"),
                explicit: false,
            });
        }
    }

    // 8. No structural match found.
    //    IntentResolver will try AI, then fall back to AppIndex → web search.
    PatternResult::NoMatch {
        input: trimmed.to_string(),
    }
}

fn try_explicit_prefix(input: &str) -> Option<Route> {
    let first_word = input.split_whitespace().next()?;
    let lower = first_word.to_lowercase();

    if KNOWN_PREFIXES.contains(&lower.as_str()) {
        let args = input[first_word.len()..].trim_start().to_string();

        // Map the prefix string to a static str
        let (handler, args) = match lower.as_str() {
            "ask" => ("ask", args),
            "bm" | "bookmark" => ("bm", args),
            "browse" => ("browse", args),
            "clip" | "clipboard" => ("clip", args),
            "emoji" => ("emoji", args),
            // App control verbs — pass verb + target as args to appctl handler
            "focus" | "quit" | "close" | "kill" => ("appctl", format!("{} {}", lower, args)),
            "open" => {
                // "open https://github.com" → redirect to url handler
                if looks_like_url(&args) {
                    ("url", args)
                } else {
                    ("open", args)
                }
            }
            "web" => ("web", args),
            "yt" => ("yt", args),
            "run" => ("run", args),
            "calc" => ("calc", args),
            "file" => ("file", args),
            "url" => ("url", args),
            "media" => ("media", args),
            "project" => ("project", args),
            "system" => ("system", args),
            "note" | "notes" => ("note", args),
            "todo" | "todos" => ("todo", args),
            "snip" | "snippet" | "snippets" => ("snip", args),
            "weather" => {
                // Strip leading "in " — "weather in tokyo" → args "tokyo"
                let weather_args = args.strip_prefix("in ").unwrap_or(&args).to_string();
                ("weather", weather_args)
            }
            "sym" => ("sym", args),
            "sysinfo" => ("sysinfo", args),
            "unicode" => ("unicode", args),
            "time" | "tz" | "clock" => {
                let time_args = args.strip_prefix("in ").unwrap_or(&args).to_string();
                ("time", time_args)
            }
            "alias" | "aliases" => ("alias", args),
            "reminder" => ("reminder", args),
            "remind" => {
                // "remind me to X in 30m" → strip "me to " prefix
                let reminder_args = args
                    .strip_prefix("me to ")
                    .or_else(|| args.strip_prefix("me "))
                    .unwrap_or(&args)
                    .to_string();
                ("reminder", format!("add {reminder_args}"))
            }
            "timer" => ("timer", args),
            "stopwatch" => {
                // Route "stopwatch [args]" → timer handler with "stopwatch [args]"
                if args.is_empty() {
                    ("timer", "stopwatch".to_string())
                } else {
                    ("timer", format!("stopwatch {args}"))
                }
            }
            // Bare shortcuts — pass the keyword itself as args
            "ip" | "cpu" | "mem" | "disk" | "temp" | "gpu" | "battery" | "net" | "audio"
            | "display" | "os" | "speedtest" => ("sysinfo", lower.clone()),
            "ctx" => ("ctx", args),
            // Power commands — single-word, unambiguous, no AI needed.
            // "shutdown in N" has trailing args — let it fall through to structured phrase handler.
            "shutdown" | "poweroff" => {
                if args.is_empty() {
                    ("system", "shutdown".to_string())
                } else {
                    return None; // "shutdown in 10m" — handled by structured phrase step
                }
            }
            "reboot" | "restart" => ("system", "reboot".to_string()),
            "lock" => ("system", "lock".to_string()),
            "suspend" | "sleep" => ("system", "suspend".to_string()),
            "hibernate" => ("system", "hibernate".to_string()),
            "logout" | "signout" => ("system", "logout".to_string()),
            // Common bare-word controls
            "mute" => ("system", "mute".to_string()),
            "unmute" => ("system", "unmute".to_string()),
            _ => return None,
        };
        Some(Route {
            handler,
            args,
            explicit: true,
        })
    } else {
        None
    }
}

fn looks_like_url(input: &str) -> bool {
    // Explicit scheme
    if input.starts_with("http://") || input.starts_with("https://") {
        return true;
    }

    // word.tld pattern (no spaces allowed in URLs)
    if input.contains(' ') {
        return false;
    }

    // Check for domain.tld pattern
    if let Some(dot_pos) = input.find('.') {
        let after_dot = &input[dot_pos..];
        // Check if it ends with a known TLD (possibly followed by / or path)
        for tld in TLDS {
            if after_dot.starts_with(tld)
                && (after_dot.len() == tld.len()
                    || after_dot.as_bytes().get(tld.len()) == Some(&b'/'))
            {
                return true;
            }
        }
    }

    false
}

fn is_math_expression(input: &str) -> bool {
    if input.is_empty() {
        return false;
    }

    let mut has_operator = false;
    let mut has_digit = false;

    for ch in input.chars() {
        match ch {
            '0'..='9' | '.' => has_digit = true,
            '+' | '-' | '*' | '/' | '^' | '%' => has_operator = true,
            '(' | ')' | ' ' => {}
            _ => return false, // Any letter or unknown char → not math
        }
    }

    // Need at least one digit and one operator
    has_digit && has_operator
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_prefixes() {
        let r = route("web rust lang").unwrap();
        assert_eq!(r.handler, "web");
        assert_eq!(r.args, "rust lang");
        assert!(r.explicit);

        let r = route("yt funny cats").unwrap();
        assert_eq!(r.handler, "yt");
        assert_eq!(r.args, "funny cats");

        let r = route("run ls -la").unwrap();
        assert_eq!(r.handler, "run");
        assert_eq!(r.args, "ls -la");

        let r = route("open firefox").unwrap();
        assert_eq!(r.handler, "open");
        assert_eq!(r.args, "firefox");
    }

    #[test]
    fn trigger_characters() {
        let r = route("=2+2").unwrap();
        assert_eq!(r.handler, "calc");
        assert_eq!(r.args, "2+2");
        assert!(r.explicit);

        let r = route("=sqrt(144)").unwrap();
        assert_eq!(r.handler, "calc");
        assert_eq!(r.args, "sqrt(144)");

        let r = route(">ls -la").unwrap();
        assert_eq!(r.handler, "run");
        assert_eq!(r.args, "ls -la");
        assert!(r.explicit);
    }

    #[test]
    fn shorthand_colon_triggers() {
        let cases: &[(&str, &str, &str)] = &[
            ("w:rust lang", "web", "rust lang"),
            ("yt:funny cats", "yt", "funny cats"),
            ("r:ls -la", "run", "ls -la"),
            ("c:sqrt(144)", "calc", "sqrt(144)"),
            ("f:~/Documents", "file", "~/Documents"),
            ("o:firefox", "open", "firefox"),
            ("bm:github", "bm", "github"),
            ("n:call dentist", "note", "call dentist"),
            ("t:add buy milk", "todo", "add buy milk"),
            ("cl:", "clip", ""),
            ("m:pause", "media", "pause"),
            ("p:lychi", "project", "lychi"),
            ("a:what is rust", "ask", "what is rust"),
            ("si:cpu", "sysinfo", "cpu"),
            ("sys:lock", "system", "lock"),
            ("e:fire", "emoji", "fire"),
            ("u:arrow", "unicode", "arrow"),
            ("sym:infinity", "sym", "infinity"),
        ];
        for &(input, expected_handler, expected_args) in cases {
            let r = route(input).unwrap();
            assert_eq!(r.handler, expected_handler, "input: {input}");
            assert_eq!(r.args, expected_args, "input: {input}");
            assert!(r.explicit, "input: {input}");
        }
    }

    #[test]
    fn file_paths() {
        let r = route("~/Documents").unwrap();
        assert_eq!(r.handler, "file");
        assert_eq!(r.args, "~/Documents");

        let r = route("/tmp").unwrap();
        assert_eq!(r.handler, "file");

        let r = route("./src").unwrap();
        assert_eq!(r.handler, "file");
    }

    #[test]
    fn urls() {
        let r = route("https://github.com").unwrap();
        assert_eq!(r.handler, "url");

        let r = route("github.com").unwrap();
        assert_eq!(r.handler, "url");

        let r = route("example.org/path").unwrap();
        assert_eq!(r.handler, "url");

        // Not a URL — has spaces → NoMatch
        assert!(matches!(
            route("not a url.com"),
            PatternResult::NoMatch { .. }
        ));
    }

    #[test]
    fn math_expressions() {
        let r = route("2+2").unwrap();
        assert_eq!(r.handler, "calc");
        assert_eq!(r.args, "2+2");

        let r = route("100/3").unwrap();
        assert_eq!(r.handler, "calc");

        let r = route("(10 + 5) * 3").unwrap();
        assert_eq!(r.handler, "calc");

        // Not math — contains letters → NoMatch
        assert!(matches!(route("2x+3"), PatternResult::NoMatch { .. }));
    }

    #[test]
    fn default_app_search() {
        // Bare app names with no structural match → NoMatch (AI or open fallback)
        assert!(matches!(route("firefox"), PatternResult::NoMatch { .. }));
        assert!(matches!(route("spotify"), PatternResult::NoMatch { .. }));
    }

    #[test]
    fn sysinfo_commands() {
        let r = route("sysinfo").unwrap();
        assert_eq!(r.handler, "sysinfo");
        assert_eq!(r.args, "");
        assert!(r.explicit);

        let r = route("sysinfo cpu").unwrap();
        assert_eq!(r.handler, "sysinfo");
        assert_eq!(r.args, "cpu");

        // Bare shortcuts pass keyword as args
        let r = route("ip").unwrap();
        assert_eq!(r.handler, "sysinfo");
        assert_eq!(r.args, "ip");

        let r = route("cpu").unwrap();
        assert_eq!(r.handler, "sysinfo");
        assert_eq!(r.args, "cpu");

        let r = route("mem").unwrap();
        assert_eq!(r.handler, "sysinfo");
        assert_eq!(r.args, "mem");

        let r = route("disk").unwrap();
        assert_eq!(r.handler, "sysinfo");
        assert_eq!(r.args, "disk");
    }

    // --- Natural language fallback tests ---
    // Natural language phrases have no structural match → PatternResult::NoMatch.
    // IntentResolver then tries AI, then falls back to open→web.

    #[test]
    fn natural_language_is_no_match() {
        for input in &[
            "pause the music",
            "play something on spotify",
            "what's my ip address",
            "how much ram is being used",
            "shut down the computer",
            "what's the weather in paris",
            "will it rain today",
            "what time is it in london",
            "find large files in downloads",
            "whats my system cpu",
            "increase the volume",
        ] {
            assert!(
                matches!(route(input), PatternResult::NoMatch { .. }),
                "expected NoMatch for: {input}"
            );
        }
    }

    #[test]
    fn explicit_media_prefix() {
        let r = route("media pause").unwrap();
        assert_eq!(r.handler, "media");
        assert_eq!(r.args, "pause");
        assert!(r.explicit);

        let r = route("media spotify pause").unwrap();
        assert_eq!(r.handler, "media");
        assert_eq!(r.args, "spotify pause");
        assert!(r.explicit);

        let r = route("media yt next").unwrap();
        assert_eq!(r.handler, "media");
        assert_eq!(r.args, "yt next");
        assert!(r.explicit);
    }

    #[test]
    fn keyword_ask() {
        // Natural language questions → NoMatch (AI handles it)
        assert!(matches!(
            route("what is the capital of France"),
            PatternResult::NoMatch { .. }
        ));
        assert!(matches!(
            route("who invented the telephone"),
            PatternResult::NoMatch { .. }
        ));
        assert!(matches!(
            route("how does photosynthesis work"),
            PatternResult::NoMatch { .. }
        ));
    }

    #[test]
    fn conversion_expressions() {
        let r = route("5 kg to lb").unwrap();
        assert_eq!(r.handler, "calc");
        assert_eq!(r.args, "5 kg to lb");
        assert!(!r.explicit);

        let r = route("100cm to inches").unwrap();
        assert_eq!(r.handler, "calc");

        let r = route("72 f to c").unwrap();
        assert_eq!(r.handler, "calc");

        let r = route("1 gal to l").unwrap();
        assert_eq!(r.handler, "calc");

        let r = route("1 gb to mb").unwrap();
        assert_eq!(r.handler, "calc");

        // Currency (routes to calc even though rates may not be cached)
        let r = route("250 usd to eur").unwrap();
        assert_eq!(r.handler, "calc");

        // Not a conversion → NoMatch
        assert!(matches!(route("5 kg"), PatternResult::NoMatch { .. }));
    }

    #[test]
    fn explicit_system_power_words() {
        // Bare power words are now explicit prefix matches — instant, no AI needed
        let r = route("shutdown").unwrap();
        assert_eq!(r.handler, "system");
        assert_eq!(r.args, "shutdown");
        assert!(r.explicit);

        let r = route("reboot").unwrap();
        assert_eq!(r.handler, "system");
        assert_eq!(r.args, "reboot");
        assert!(r.explicit);

        let r = route("lock").unwrap();
        assert_eq!(r.handler, "system");
        assert_eq!(r.args, "lock");
        assert!(r.explicit);

        let r = route("suspend").unwrap();
        assert_eq!(r.handler, "system");
        assert_eq!(r.args, "suspend");
        assert!(r.explicit);

        let r = route("hibernate").unwrap();
        assert_eq!(r.handler, "system");
        assert_eq!(r.args, "hibernate");
        assert!(r.explicit);

        let r = route("logout").unwrap();
        assert_eq!(r.handler, "system");
        assert_eq!(r.args, "logout");
        assert!(r.explicit);

        let r = route("mute").unwrap();
        assert_eq!(r.handler, "system");
        assert_eq!(r.args, "mute");
        assert!(r.explicit);

        let r = route("unmute").unwrap();
        assert_eq!(r.handler, "system");
        assert_eq!(r.args, "unmute");
        assert!(r.explicit);

        // Aliases
        let r = route("poweroff").unwrap();
        assert_eq!(r.handler, "system");
        assert_eq!(r.args, "shutdown");

        let r = route("restart").unwrap();
        assert_eq!(r.handler, "system");
        assert_eq!(r.args, "reboot");

        let r = route("sleep").unwrap();
        assert_eq!(r.handler, "system");
        assert_eq!(r.args, "suspend");
    }

    #[test]
    fn structured_power_phrases() {
        // "shutdown in N" and "cancel shutdown" are handled deterministically
        let r = route("shutdown in 10 minutes").unwrap();
        assert_eq!(r.handler, "system");
        assert!(r.args.contains("shutdown in"));

        let r = route("cancel shutdown").unwrap();
        assert_eq!(r.handler, "system");
        assert_eq!(r.args, "cancel shutdown");
    }

    #[test]
    fn explicit_time_prefix() {
        // Colon trigger
        let r = route("tz:tokyo").unwrap();
        assert_eq!(r.handler, "time");
        assert_eq!(r.args, "tokyo");
        assert!(r.explicit);

        // Explicit prefix
        let r = route("time tokyo").unwrap();
        assert_eq!(r.handler, "time");
        assert_eq!(r.args, "tokyo");
        assert!(r.explicit);

        // Duration conversion still goes to calc
        let r = route("2 hours to minutes").unwrap();
        assert_eq!(r.handler, "calc");
    }

    #[test]
    fn fallback_behaviour() {
        // Bare app names → NoMatch (IntentResolver tries AI then open→web)
        assert!(matches!(route("firefox"), PatternResult::NoMatch { .. }));
        assert!(matches!(
            route("find large files in downloads"),
            PatternResult::NoMatch { .. }
        ));

        // Explicit prefix still wins
        let r = route("weather london").unwrap();
        assert_eq!(r.handler, "weather");
        assert!(r.explicit);
    }

    #[test]
    fn question_trigger_prefix() {
        // ? prefix forces web search
        let r = route("? whats my system memory").unwrap();
        assert_eq!(r.handler, "web");
        assert_eq!(r.args, "whats my system memory");
        assert!(r.explicit);

        let r = route("?rust programming").unwrap();
        assert_eq!(r.handler, "web");
        assert_eq!(r.args, "rust programming");
        assert!(r.explicit);
    }

    #[test]
    fn empty_input_is_no_match() {
        assert!(matches!(route(""), PatternResult::NoMatch { .. }));
        assert!(matches!(route("   "), PatternResult::NoMatch { .. }));
    }
}

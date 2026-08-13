//! Smart input router — deterministic pre-filter for intent routing.
//!
//! Routes user input to the appropriate handler based on:
//! 1. Explicit prefixes (`web `, `yt `, `run `, `open `, `calc `, etc.)
//! 2. Trigger characters (`?` web search, `=` calc, `>` shell)
//! 3. Pattern detection (file paths, URLs, math/conversion expressions)
//! 4. No match: `PatternResult::NoMatch` — IntentResolver uses AI, falls back to open→web

use crate::action_registry::registry::ActionRegistry;

/// Shorthand colon-prefix triggers → handler id. Longest match wins (multi-char
/// prefixes are checked before single-char). This is the single source of truth
/// for both routing (see `route`) and the dynamic Guide's Triggers list, so the
/// two never drift.
pub static COLON_TRIGGERS: &[(&str, &str)] = &[
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
    ("m:", "media"),
    ("p:", "project"),
    ("tz:", "time"),
    ("al:", "alias"),
    ("sn:", "snip"),
    ("tm:", "timer"),
    ("rm:", "reminder"),
];

/// The structural (character-sigil) triggers — input shapes that route without a
/// keyword. These are truly structural (defined in code below), so they're a
/// small fixed set paired here with a human description for the Guide.
pub static SIGIL_TRIGGERS: &[(&str, &str)] = &[
    ("=", "Evaluate a math or unit/currency expression"),
    (">", "Run a shell command"),
    ("/", "Fuzzy-search files"),
    ("~/", "Open a path from home"),
    ("@", "Reference a file inside a command"),
    ("#hex", "Preview a hex color"),
    ("example.com", "Open a URL"),
];

/// Common TLDs for URL detection.
const TLDS: &[&str] = &[
    ".com", ".org", ".net", ".io", ".dev", ".app", ".co", ".me", ".info", ".xyz", ".edu", ".gov",
    ".mil", ".int", ".eu", ".uk", ".de", ".fr", ".jp", ".cn", ".au", ".ca", ".br", ".in", ".ru",
    ".ly", ".ai", ".gg", ".tv", ".cc",
];

/// Confidence level of a deterministic pattern match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Confidence {
    /// Explicit prefix or trigger character (`web `, `=`, `>`, `?`, colon-triggers).
    Explicit,
    /// Strong structural signal: URL, absolute/relative file path, math expression,
    /// unit/currency conversion, known power phrase.
    Strong,
    /// Weak heuristic: home-directory filename match (no explicit prefix).
    Weak,
}

/// A deterministically matched route from patterns.rs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    /// Handler to dispatch to (e.g. "web", "calc", "run", "file", "url")
    pub handler: String,
    /// Arguments to pass to the handler
    pub args: String,
    /// Whether this route was explicitly triggered (prefix or trigger char).
    /// Kept for RoutingMethod selection in IntentResolver.
    pub explicit: bool,
    /// Confidence level of this match.
    pub confidence: Confidence,
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

/// Route raw user input to the appropriate handler.
pub fn route(raw: &str, registry: &ActionRegistry) -> PatternResult {
    route_inner(raw, true, registry)
}

fn route_inner(raw: &str, check_aliases: bool, registry: &ActionRegistry) -> PatternResult {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return PatternResult::NoMatch {
            input: String::new(),
        };
    }

    // 1. Explicit prefix — first word matches a known handler
    if let Some(r) = try_explicit_prefix(trimmed, registry) {
        return PatternResult::Match(r);
    }

    // 2. Trigger characters & shorthand colon-prefixes
    //    Multi-char prefixes checked first (longest match wins), then single-char.
    for &(prefix, handler) in COLON_TRIGGERS {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            return PatternResult::Match(Route {
                handler: handler.to_string(),
                args: rest.trim().to_string(),
                explicit: true,
                confidence: Confidence::Explicit,
            });
        }
    }
    if let Some(query) = trimmed.strip_prefix('?') {
        return PatternResult::Match(Route {
            handler: "web".to_string(),
            args: query.trim().to_string(),
            explicit: true,
            confidence: Confidence::Explicit,
        });
    }
    if let Some(expr) = trimmed.strip_prefix('=') {
        return PatternResult::Match(Route {
            handler: "calc".to_string(),
            args: expr.trim().to_string(),
            explicit: true,
            confidence: Confidence::Explicit,
        });
    }
    if let Some(cmd) = trimmed.strip_prefix('>') {
        return PatternResult::Match(Route {
            handler: "run".to_string(),
            args: cmd.trim().to_string(),
            explicit: true,
            confidence: Confidence::Explicit,
        });
    }

    // 3. File path
    if trimmed.starts_with('/') || trimmed.starts_with("~/") || trimmed.starts_with("./") {
        return PatternResult::Match(Route {
            handler: "file".to_string(),
            args: trimmed.to_string(),
            explicit: false,
            confidence: Confidence::Strong,
        });
    }

    // 4. URL
    if looks_like_url(trimmed) {
        return PatternResult::Match(Route {
            handler: "url".to_string(),
            args: trimmed.to_string(),
            explicit: false,
            confidence: Confidence::Strong,
        });
    }

    // 5. Math expression (digits + operators only, no letters)
    if is_math_expression(trimmed) {
        return PatternResult::Match(Route {
            handler: "calc".to_string(),
            args: trimmed.to_string(),
            explicit: false,
            confidence: Confidence::Strong,
        });
    }

    // 5b. Unit/currency conversion (e.g. "5 kg to lb", "100 usd to eur")
    if crate::action_registry::handlers::calc::is_conversion_expression(trimmed) {
        return PatternResult::Match(Route {
            handler: "calc".to_string(),
            args: trimmed.to_string(),
            explicit: false,
            confidence: Confidence::Strong,
        });
    }

    // 5c. Bare hex color (e.g. "#FF5733", "#F53") — route to color handler
    if let Some(hex) = trimmed.strip_prefix('#')
        && matches!(hex.len(), 3 | 6 | 8)
        && hex.chars().all(|c| c.is_ascii_hexdigit())
    {
        return PatternResult::Match(Route {
            handler: "color".to_string(),
            args: trimmed.to_string(),
            explicit: false,
            confidence: Confidence::Strong,
        });
    }

    // 5d. Structured power phrases — unambiguous, handle before AI fallback
    let lower = trimmed.to_lowercase();
    if lower.starts_with("shutdown in ") || lower.starts_with("shut down in ") {
        return PatternResult::Match(Route {
            handler: "system".to_string(),
            args: lower,
            explicit: false,
            confidence: Confidence::Strong,
        });
    }
    if lower == "cancel shutdown" || lower == "shutdown cancel" {
        return PatternResult::Match(Route {
            handler: "system".to_string(),
            args: "cancel shutdown".to_string(),
            explicit: false,
            confidence: Confidence::Strong,
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
            return route_inner(&full, false, registry);
        }
    }

    // 7. Home directory entry (e.g. "Downloads", "Documents", "report.pdf")
    if !trimmed.contains(' ')
        && let Some(home) = dirs::home_dir()
    {
        let candidate = home.join(trimmed);
        if candidate.exists() {
            return PatternResult::Match(Route {
                handler: "file".to_string(),
                args: format!("~/{trimmed}"),
                explicit: false,
                confidence: Confidence::Weak,
            });
        }
    }

    // 8. No structural match found.
    //    IntentResolver will try AI, then fall back to AppIndex → web search.
    PatternResult::NoMatch {
        input: trimmed.to_string(),
    }
}

fn try_explicit_prefix(input: &str, registry: &ActionRegistry) -> Option<Route> {
    let first_word = input.split_whitespace().next()?;
    let lower = first_word.to_lowercase();
    let args = input[first_word.len()..].trim_start().to_string();

    // Conditional / two-step cases that a single handler `triggers()` declaration
    // can't express — they must run BEFORE the registry's data-driven routing and
    // return early. Everything else is resolved by `registry.route_prefix` below.
    let conditional: Option<(&'static str, String)> = match lower.as_str() {
        "open" => {
            // "open https://github.com" → url handler.
            // "open ~/Pictures/x.png" (a real file path) → file handler, so the
            // user doesn't need to know apps use `open` and files use `file`.
            // Otherwise treat as an app name.
            if looks_like_url(&args) {
                Some(("url", args.clone()))
            } else if looks_like_path(&args) {
                Some(("file", args.clone()))
            } else {
                Some(("open", args.clone()))
            }
        }
        // Unit conversion alias — routes to the calc handler. (The old AI
        // "convert clipboard" transform is now an AI preset, not a handler.)
        "convert" => Some(("calc", args.clone())),
        // Power commands with trailing args ("shutdown in N") fall through to the
        // structured-phrase step; a bare word routes via the registry (system).
        "shutdown" | "poweroff" => {
            if args.is_empty() {
                None // bare word — let registry.route_prefix handle it below
            } else {
                return None; // "shutdown in 10m" — handled by structured phrase step
            }
        }
        // "remind me to X in 30m" — strip "me to "/"me " then prepend "add ".
        // Two-step transform, kept structural.
        "remind" => {
            let reminder_args = args
                .strip_prefix("me to ")
                .or_else(|| args.strip_prefix("me "))
                .unwrap_or(&args)
                .to_string();
            Some(("reminder", format!("add {reminder_args}")))
        }
        _ => None,
    };
    if let Some((handler, args)) = conditional {
        return Some(Route {
            handler: handler.to_string(),
            args,
            explicit: true,
            confidence: Confidence::Explicit,
        });
    }

    // Data-driven routing: every remaining keyword prefix comes from a handler's
    // declared `triggers()`, indexed by the registry.
    registry
        .route_prefix(&lower, &args)
        .map(|(handler, args)| Route {
            handler,
            args,
            explicit: true,
            confidence: Confidence::Explicit,
        })
}

/// Whether `input` is a filesystem path we should hand to the `file` handler.
/// Path-shaped prefixes (`/`, `~/`, `./`, `../`) always qualify; anything else
/// only qualifies if it actually exists on disk (adaptive — a real file the user
/// referenced via `@` counts even without a leading slash).
fn looks_like_path(input: &str) -> bool {
    let s = input.trim();
    if s.is_empty() {
        return false;
    }
    if s.starts_with('/') || s.starts_with("~/") || s.starts_with("./") || s.starts_with("../") {
        return true;
    }
    std::path::Path::new(s).exists()
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
    use crate::action_registry::registry::ActionRegistry;
    use crate::action_registry::{ActionHandler, ActionResult, Trigger};
    use crate::error::LychiError;
    use async_trait::async_trait;

    #[test]
    fn looks_like_path_accepts_path_prefixes() {
        assert!(looks_like_path("/tmp/x"));
        assert!(looks_like_path("~/Pictures/x.png"));
        assert!(looks_like_path("./rel"));
        assert!(looks_like_path("../up"));
    }

    #[test]
    fn looks_like_path_rejects_plain_app_names() {
        assert!(!looks_like_path("firefox"));
        assert!(!looks_like_path("spotify"));
        assert!(!looks_like_path(""));
    }

    /// Minimal handler that only carries an id + trigger declaration — enough to
    /// drive the router's prefix index in unit tests without pulling in every real
    /// handler's constructor dependencies (db, AI provider, timer state, …).
    struct TestHandler {
        id: &'static str,
        triggers: &'static [Trigger],
    }

    #[async_trait]
    impl ActionHandler for TestHandler {
        fn id(&self) -> &str {
            self.id
        }
        fn description(&self) -> &str {
            "test"
        }
        fn triggers(&self) -> &'static [Trigger] {
            self.triggers
        }
        async fn execute(
            &self,
            _ctx: &crate::action_registry::ExecContext,
            _args: &str,
        ) -> Result<ActionResult, LychiError> {
            Ok(ActionResult::default())
        }
    }

    /// A registry populated with the real handlers' trigger declarations, so the
    /// router's keyword routing is exercised end-to-end. Kept in sync with the
    /// `triggers()` each handler declares.
    fn test_registry() -> ActionRegistry {
        use crate::action_registry::{ArgTransform, Trigger};
        macro_rules! h {
            ($id:expr, $triggers:expr) => {
                Box::new(TestHandler {
                    id: $id,
                    // Test-only: leak the trigger table to get a 'static slice
                    // without hand-declaring a named static per handler.
                    triggers: Box::leak($triggers.to_vec().into_boxed_slice()),
                })
            };
        }
        let mut r = ActionRegistry::new();
        r.register(h!("web", &[Trigger::keywords(&["web"])]));
        r.register(h!("yt", &[Trigger::keywords(&["yt"])]));
        r.register(h!("run", &[Trigger::keywords(&["run"])]));
        r.register(h!("calc", &[Trigger::keywords(&["calc"])]));
        r.register(h!("define", &[Trigger::keywords(&["define"])]));
        r.register(h!("file", &[Trigger::keywords(&["file"])]));
        r.register(h!("url", &[Trigger::keywords(&["url"])]));
        r.register(h!("media", &[Trigger::keywords(&["media"])]));
        r.register(h!("project", &[Trigger::keywords(&["project"])]));
        r.register(h!("ask", &[Trigger::keywords(&["ask"])]));
        r.register(h!("bm", &[Trigger::keywords(&["bm", "bookmark"])]));
        r.register(h!("browse", &[Trigger::keywords(&["browse"])]));
        r.register(h!("clip", &[Trigger::keywords(&["clip", "clipboard"])]));
        r.register(h!("clear", &[Trigger::keywords(&["clear"])]));
        r.register(h!("emoji", &[Trigger::keywords(&["emoji"])]));
        r.register(h!("sym", &[Trigger::keywords(&["sym"])]));
        r.register(h!("unicode", &[Trigger::keywords(&["unicode"])]));
        r.register(h!("ctx", &[Trigger::keywords(&["ctx"])]));
        r.register(h!("ssh", &[Trigger::keywords(&["ssh"])]));
        r.register(h!("color", &[Trigger::keywords(&["color", "colour"])]));
        r.register(h!(
            "win",
            &[Trigger::keywords(&["win", "window", "windows"])]
        ));
        r.register(h!("resize", &[Trigger::keywords(&["resize"])]));
        r.register(h!("qr", &[Trigger::keywords(&["qr"])]));
        r.register(h!("reminder", &[Trigger::keywords(&["reminder"])]));
        r.register(h!("alias", &[Trigger::keywords(&["alias", "aliases"])]));
        r.register(h!("note", &[Trigger::keywords(&["note", "notes"])]));
        r.register(h!("todo", &[Trigger::keywords(&["todo", "todos"])]));
        r.register(h!(
            "snip",
            &[Trigger::keywords(&["snip", "snippet", "snippets"])]
        ));
        r.register(h!(
            "screenshot",
            &[Trigger::keywords(&[
                "screenshot",
                "screencap",
                "screengrab"
            ])]
        ));
        r.register(h!("services", &[Trigger::keywords(&["services"])]));
        r.register(h!(
            "service",
            &[Trigger::keywords(&["service", "systemctl"])]
        ));
        r.register(h!(
            "time",
            &[Trigger::new(
                &["time", "tz", "clock"],
                ArgTransform::StripLeading("in ")
            )]
        ));
        r.register(h!(
            "weather",
            &[Trigger::new(
                &["weather"],
                ArgTransform::StripLeading("in ")
            )]
        ));
        r.register(h!(
            "appctl",
            &[
                Trigger::keywords(&["appctl"]),
                Trigger::new(
                    &["focus", "quit", "close", "kill"],
                    ArgTransform::PrependKeyword
                ),
            ]
        ));
        r.register(h!(
            "pin_workspace",
            &[
                Trigger::new(&["pin"], ArgTransform::StripLeading("workspace ")),
                Trigger::new(&["unpin"], ArgTransform::Fixed("clear")),
            ]
        ));
        r.register(h!(
            "timer",
            &[
                Trigger::keywords(&["timer"]),
                Trigger::new(&["stopwatch"], ArgTransform::Prepend("stopwatch")),
            ]
        ));
        r.register(h!(
            "generate",
            &[
                Trigger::keywords(&["generate", "gen"]),
                Trigger::new(&["password"], ArgTransform::Prepend("password")),
                Trigger::new(&["uuid"], ArgTransform::Fixed("uuid")),
                Trigger::new(&["token"], ArgTransform::Prepend("token")),
                Trigger::new(&["random", "rand"], ArgTransform::Prepend("random")),
            ]
        ));
        r.register(h!(
            "devutil",
            &[Trigger::new(
                &[
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
                ],
                ArgTransform::PrependKeyword
            )]
        ));
        r.register(h!(
            "sysinfo",
            &[
                Trigger::keywords(&["sysinfo"]),
                Trigger::new(
                    &[
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
                    ],
                    ArgTransform::KeywordOnly
                ),
            ]
        ));
        r.register(h!(
            "system",
            &[
                Trigger::keywords(&["system"]),
                Trigger::new(&["shutdown", "poweroff"], ArgTransform::Fixed("shutdown")),
                Trigger::new(&["reboot", "restart"], ArgTransform::Fixed("reboot")),
                Trigger::new(&["lock"], ArgTransform::Fixed("lock")),
                Trigger::new(&["suspend", "sleep"], ArgTransform::Fixed("suspend")),
                Trigger::new(&["hibernate"], ArgTransform::Fixed("hibernate")),
                Trigger::new(&["logout", "signout"], ArgTransform::Fixed("logout")),
                Trigger::new(&["mute"], ArgTransform::Fixed("mute")),
                Trigger::new(&["unmute"], ArgTransform::Fixed("unmute")),
            ]
        ));
        r.register(h!(
            "packages",
            &[
                Trigger::new(&["install"], ArgTransform::Prepend("install")),
                Trigger::keywords(&["pkg", "package"]),
            ]
        ));
        r
    }

    /// Test-only wrapper so existing tests can call `route(input)` unchanged.
    fn route(raw: &str) -> PatternResult {
        super::route(raw, &test_registry())
    }

    /// `ssh <host>` must reach the ssh handler (which opens a terminal), NEVER
    /// the web handler. A tester once saw "ssh opens a browser" — the input was
    /// routing to `web`. This pins that the ssh trigger wins for every common
    /// host shape (bare alias, user@host, an IP, a dotted hostname), so a future
    /// URL/web heuristic can't quietly steal it back.
    #[test]
    fn ssh_host_routes_to_ssh_not_web() {
        for inp in [
            "ssh nimbus",
            "ssh myserver",
            "ssh user@host.com",
            "ssh 10.0.0.5",
            "ssh box.example.com",
        ] {
            let r = route(inp).unwrap();
            assert_eq!(
                r.handler, "ssh",
                "`{inp}` must route to ssh, got `{}`",
                r.handler
            );
        }
    }

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
            ("cl:", "clip", ""),
            ("m:pause", "media", "pause"),
            ("p:lychi", "project", "lychi"),
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
    fn kill_and_appctl_route_to_appctl_not_open() {
        // "kill spotify" → appctl with the verb+target.
        let r = route("kill spotify").unwrap();
        assert_eq!(r.handler, "appctl");
        assert_eq!(r.args, "kill spotify");

        // The picker's internal run command must route back to appctl verbatim —
        // NOT fall through to a NoMatch that the app index would turn into "open
        // spotify" (the bug: `kill spotify` focused Spotify instead of killing it).
        let r = route("appctl kill spotify").unwrap();
        assert_eq!(r.handler, "appctl");
        assert_eq!(r.args, "kill spotify");
        assert!(r.explicit);

        let r = route("appctl kill 12345").unwrap();
        assert_eq!(r.handler, "appctl");
        assert_eq!(r.args, "kill 12345");
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

    #[test]
    fn text_verbs_route_to_devutil() {
        for (input, expected_args) in [
            ("upper hello", "upper hello"),
            ("slug My Post", "slug My Post"),
            ("json {\"a\":1}", "json {\"a\":1}"),
            ("reverse abc", "reverse abc"),
            ("count one two", "count one two"),
        ] {
            let r = route(input).unwrap();
            assert_eq!(r.handler, "devutil", "input: {input}");
            assert_eq!(r.args, expected_args, "input: {input}");
        }
    }

    #[test]
    fn random_routes_to_generate() {
        let r = route("random 1 100").unwrap();
        assert_eq!(r.handler, "generate");
        assert_eq!(r.args, "random 1 100");
        // bare `random` still routes (defaults applied in the handler)
        let r = route("random").unwrap();
        assert_eq!(r.handler, "generate");
        assert_eq!(r.args, "random");
    }

    #[test]
    fn screenshot_routes_with_mode() {
        let r = route("screenshot").unwrap();
        assert_eq!(r.handler, "screenshot");
        assert_eq!(r.args, "");
        let r = route("screenshot area").unwrap();
        assert_eq!(r.handler, "screenshot");
        assert_eq!(r.args, "area");
        // aliases route too
        assert_eq!(route("screencap window").unwrap().handler, "screenshot");
        assert_eq!(route("screengrab").unwrap().handler, "screenshot");
    }

    #[test]
    fn services_and_service_route_correctly() {
        // Plural word lists; singular controls.
        assert_eq!(route("services").unwrap().handler, "services");
        let r = route("service nginx restart").unwrap();
        assert_eq!(r.handler, "service");
        assert_eq!(r.args, "nginx restart");
        // systemctl is an alias for the control handler.
        assert_eq!(route("systemctl docker status").unwrap().handler, "service");
    }

    #[test]
    fn package_commands_route() {
        // `install <pkg>` → packages with the verb prepended.
        let r = route("install neovim").unwrap();
        assert_eq!(r.handler, "packages");
        assert_eq!(r.args, "install neovim");
        // `pkg` namespace passes the verb through in args.
        let r = route("pkg search ripgrep").unwrap();
        assert_eq!(r.handler, "packages");
        assert_eq!(r.args, "search ripgrep");
        assert_eq!(route("package install fd").unwrap().handler, "packages");
        // Bare `search` is NOT a package command (avoids shadowing web search).
        assert!(!matches!(
            route("search cat videos"),
            PatternResult::Match(ref r) if r.handler == "packages"
        ));
    }
}

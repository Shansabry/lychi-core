//! Smart input router — Alfred-style hybrid routing.
//!
//! Routes user input to the appropriate handler based on:
//! 1. Explicit prefixes (`web `, `yt `, `run `, `open `, `calc `)
//! 2. Trigger characters (`=` for calc, `>` for shell)
//! 3. Pattern detection (file paths, URLs, math expressions)
//! 4. Keyword detection (natural language → deterministic routing)
//! 5. Default: app search → web search fallback

/// Known handler prefixes.
const KNOWN_PREFIXES: &[&str] = &[
    "ask",
    "browse",
    "open",
    "web",
    "yt",
    "run",
    "calc",
    "file",
    "url",
    "spotify",
    "project",
    "system",
    "note",
    "notes",
    "todo",
    "todos",
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
];

/// Common TLDs for URL detection.
const TLDS: &[&str] = &[
    ".com", ".org", ".net", ".io", ".dev", ".app", ".co", ".me", ".info", ".xyz", ".edu", ".gov",
    ".mil", ".int", ".eu", ".uk", ".de", ".fr", ".jp", ".cn", ".au", ".ca", ".br", ".in", ".ru",
    ".ly", ".ai", ".gg", ".tv", ".cc",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    /// Handler prefix to dispatch to (e.g. "open", "web", "calc", "run", "file", "url")
    pub handler: &'static str,
    /// Arguments to pass to the handler
    pub args: String,
    /// Whether this route was explicitly triggered (prefix or trigger char)
    pub explicit: bool,
}

/// Route raw user input to the appropriate handler.
pub fn route(raw: &str) -> Route {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Route {
            handler: "open",
            args: String::new(),
            explicit: false,
        };
    }

    // 1. Explicit prefix — first word matches a known handler
    if let Some(r) = try_explicit_prefix(trimmed) {
        return r;
    }

    // 2. Trigger characters
    if let Some(expr) = trimmed.strip_prefix('=') {
        return Route {
            handler: "calc",
            args: expr.trim().to_string(),
            explicit: true,
        };
    }
    if let Some(cmd) = trimmed.strip_prefix('>') {
        return Route {
            handler: "run",
            args: cmd.trim().to_string(),
            explicit: true,
        };
    }

    // 3. File path
    if trimmed.starts_with('/') || trimmed.starts_with("~/") || trimmed.starts_with("./") {
        return Route {
            handler: "file",
            args: trimmed.to_string(),
            explicit: false,
        };
    }

    // 4. URL
    if looks_like_url(trimmed) {
        return Route {
            handler: "url",
            args: trimmed.to_string(),
            explicit: false,
        };
    }

    // 5. Math expression (digits + operators only, no letters)
    if is_math_expression(trimmed) {
        return Route {
            handler: "calc",
            args: trimmed.to_string(),
            explicit: false,
        };
    }

    // 6. Keyword routing — common natural language patterns
    if let Some(r) = try_keyword_route(trimmed) {
        return r;
    }

    // 7. Home directory entry (e.g. "Downloads", "Documents", "report.pdf")
    if !trimmed.contains(' ')
        && let Some(home) = dirs::home_dir()
    {
        let candidate = home.join(trimmed);
        if candidate.exists() {
            return Route {
                handler: "file",
                args: format!("~/{trimmed}"),
                explicit: false,
            };
        }
    }

    // 8. Default — app search
    Route {
        handler: "open",
        args: trimmed.to_string(),
        explicit: false,
    }
}

/// Words that indicate natural language rather than a direct command argument.
const FILLER_WORDS: &[&str] = &[
    "the", "my", "a", "an", "this", "that", "please", "can", "you",
];

fn try_explicit_prefix(input: &str) -> Option<Route> {
    let first_word = input.split_whitespace().next()?;
    let lower = first_word.to_lowercase();

    if KNOWN_PREFIXES.contains(&lower.as_str()) {
        let args = input[first_word.len()..].trim_start().to_string();

        // For "open", check if the args look like natural language rather than
        // a direct app name. e.g. "open the download folder" → AI, "open firefox" → explicit.
        if lower == "open" && !args.is_empty() {
            let second_word = args.split_whitespace().next().unwrap_or("");
            if FILLER_WORDS.contains(&second_word.to_lowercase().as_str()) {
                return None; // Let it fall through to AI routing
            }
        }

        // Map the prefix string to a static str
        let (handler, args) = match lower.as_str() {
            "ask" => ("ask", args),
            "browse" => ("browse", args),
            "open" => ("open", args),
            "web" => ("web", args),
            "yt" => ("yt", args),
            "run" => ("run", args),
            "calc" => ("calc", args),
            "file" => ("file", args),
            "url" => ("url", args),
            "spotify" => ("spotify", args),
            "project" => ("project", args),
            "system" => ("system", args),
            "note" | "notes" => ("note", args),
            "todo" | "todos" => ("todo", args),
            "weather" => {
                // Strip leading "in " — "weather in tokyo" → args "tokyo"
                let weather_args = args.strip_prefix("in ").unwrap_or(&args).to_string();
                ("weather", weather_args)
            }
            "sysinfo" => ("sysinfo", args),
            // Bare shortcuts — pass the keyword itself as args
            "ip" | "cpu" | "mem" | "disk" | "temp" | "gpu" | "battery" | "net" | "audio"
            | "display" | "os" | "speedtest" => ("sysinfo", lower.clone()),
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

/// Keyword-based routing for common natural language patterns.
/// Catches phrases that would otherwise fall through to AI, saving tokens and latency.
fn try_keyword_route(input: &str) -> Option<Route> {
    let lower = input.to_lowercase();

    // --- system power commands ---
    if lower.contains("shut down") || lower.contains("shutdown") || lower.contains("power off") {
        return Some(Route {
            handler: "system",
            args: "shutdown".into(),
            explicit: false,
        });
    }
    if lower == "reboot" || lower.contains("restart") || lower.starts_with("reboot ") {
        return Some(Route {
            handler: "system",
            args: "reboot".into(),
            explicit: false,
        });
    }
    if lower.contains("lock screen") || lower.contains("lock my screen") || lower == "lock" {
        return Some(Route {
            handler: "system",
            args: "lock".into(),
            explicit: false,
        });
    }
    if (lower.contains("sleep") || lower.contains("suspend"))
        && (lower.contains("computer") || lower.contains("pc") || lower.contains("put"))
    {
        return Some(Route {
            handler: "system",
            args: "suspend".into(),
            explicit: false,
        });
    }
    if lower.contains("log out") || lower.contains("logout") || lower.contains("sign out") {
        return Some(Route {
            handler: "system",
            args: "logout".into(),
            explicit: false,
        });
    }
    if lower == "hibernate" || (lower.contains("hibernate") && lower.contains("computer")) {
        return Some(Route {
            handler: "system",
            args: "hibernate".into(),
            explicit: false,
        });
    }

    // --- media / spotify playback ---
    if lower.contains("spotify") {
        let verb = extract_media_verb(&lower);
        return Some(Route {
            handler: "spotify",
            args: verb.into(),
            explicit: false,
        });
    }
    if lower.contains("pause everything")
        || lower.contains("stop all")
        || lower.contains("pause all")
    {
        return Some(Route {
            handler: "media",
            args: "pause all".into(),
            explicit: false,
        });
    }
    if is_media_phrase(&lower) {
        let verb = extract_media_verb(&lower);
        return Some(Route {
            handler: "media",
            args: verb.into(),
            explicit: false,
        });
    }

    // --- sysinfo ---
    if lower.contains("ip address") || lower.contains("my ip") {
        return Some(Route {
            handler: "sysinfo",
            args: "ip".into(),
            explicit: false,
        });
    }
    if lower.contains("ram") || lower.contains("memory usage") || lower.contains("how much memory")
    {
        return Some(Route {
            handler: "sysinfo",
            args: "mem".into(),
            explicit: false,
        });
    }
    if lower.contains("cpu usage") || lower.contains("cpu info") || lower.contains("processor") {
        return Some(Route {
            handler: "sysinfo",
            args: "cpu".into(),
            explicit: false,
        });
    }
    if lower.contains("disk usage")
        || lower.contains("disk space")
        || lower.contains("storage space")
    {
        return Some(Route {
            handler: "sysinfo",
            args: "disk".into(),
            explicit: false,
        });
    }
    if lower.contains("temperature")
        || lower.contains("how hot")
        || lower.contains("sensor")
        || lower.contains("thermals")
    {
        return Some(Route {
            handler: "sysinfo",
            args: "temp".into(),
            explicit: false,
        });
    }
    if lower.contains("gpu") || lower.contains("graphics card") || lower.contains("video card") {
        return Some(Route {
            handler: "sysinfo",
            args: "gpu".into(),
            explicit: false,
        });
    }
    if lower.contains("battery") || lower.contains("charge level") || lower.contains("power level")
    {
        return Some(Route {
            handler: "sysinfo",
            args: "battery".into(),
            explicit: false,
        });
    }
    if lower.contains("wifi")
        || lower.contains("wi-fi")
        || lower.contains("network info")
        || lower.contains("connection info")
        || lower.contains("public ip")
        || lower.contains("internet")
    {
        return Some(Route {
            handler: "sysinfo",
            args: "net".into(),
            explicit: false,
        });
    }
    if lower.contains("volume")
        || lower.contains("audio")
        || lower.contains("sound")
        || lower.contains("speaker")
    {
        return Some(Route {
            handler: "sysinfo",
            args: "audio".into(),
            explicit: false,
        });
    }
    if lower.contains("monitor")
        || lower.contains("screen resolution")
        || lower.contains("display info")
        || lower.contains("refresh rate")
    {
        return Some(Route {
            handler: "sysinfo",
            args: "display".into(),
            explicit: false,
        });
    }
    if lower.contains("what os")
        || lower.contains("which distro")
        || lower.contains("kernel version")
        || lower.contains("linux version")
    {
        return Some(Route {
            handler: "sysinfo",
            args: "os".into(),
            explicit: false,
        });
    }
    if lower.contains("speed test")
        || lower.contains("internet speed")
        || lower.contains("download speed")
        || lower.contains("upload speed")
        || lower.contains("bandwidth")
        || lower.contains("how fast is my")
    {
        return Some(Route {
            handler: "sysinfo",
            args: "speedtest".into(),
            explicit: false,
        });
    }

    // --- weather (structured queries → weather, conversational → weather-ask) ---
    if let Some(r) = try_weather_route(&lower, input) {
        return Some(r);
    }

    // --- todo / task management ---
    if lower.starts_with("remind me to ") {
        let text = input["remind me to ".len()..].trim();
        return Some(Route {
            handler: "todo",
            args: format!("add {text}"),
            explicit: false,
        });
    }
    if lower.starts_with("add to my list") {
        let text = input.get("add to my list".len()..).unwrap_or("").trim();
        let text = text.strip_prefix(':').unwrap_or(text).trim();
        return Some(Route {
            handler: "todo",
            args: format!("add {text}"),
            explicit: false,
        });
    }
    if lower.contains("on my plate")
        || lower.contains("left to do")
        || lower.contains("did i forget")
    {
        return Some(Route {
            handler: "todo",
            args: "summary".into(),
            explicit: false,
        });
    }
    if lower == "show my todos" || lower == "my todos" {
        return Some(Route {
            handler: "todo",
            args: "list".into(),
            explicit: false,
        });
    }

    // --- notes ---
    for prefix in &["jot down", "write down", "note down"] {
        if lower.starts_with(prefix) {
            let rest = input[prefix.len()..].trim();
            let rest = rest.strip_prefix(':').unwrap_or(rest).trim();
            if !rest.is_empty() {
                return Some(Route {
                    handler: "note",
                    args: rest.to_string(),
                    explicit: false,
                });
            }
        }
    }
    if lower.contains("what did i write") || lower == "read my note" || lower == "read my notes" {
        return Some(Route {
            handler: "note",
            args: "read".into(),
            explicit: false,
        });
    }

    // --- ask — only unambiguous question starters ---
    // Broad "what/how/who" matching is too risky (grabs file queries, commands, etc.)
    // Only catch clear knowledge-seeking patterns; let AI handle the rest.
    if lower.starts_with("explain ") || lower.starts_with("define ") {
        return Some(Route {
            handler: "ask",
            args: input.to_string(),
            explicit: false,
        });
    }

    None
}

/// Check if the input looks like a media playback command.
fn is_media_phrase(lower: &str) -> bool {
    let has_verb = lower.contains("pause")
        || lower.contains("play")
        || lower.contains("skip")
        || lower.contains("next song")
        || lower.contains("previous")
        || lower.contains("stop");
    let has_context = lower.contains("music")
        || lower.contains("song")
        || lower.contains("media")
        || lower.contains("everything")
        || lower.contains("all")
        || lower.contains("track");
    has_verb && has_context
}

/// Extract the media verb from a phrase.
fn extract_media_verb(lower: &str) -> &'static str {
    if lower.contains("pause") || lower.contains("stop") {
        "pause"
    } else if lower.contains("next") || lower.contains("skip") {
        "next"
    } else if lower.contains("prev") {
        "prev"
    } else {
        "play"
    }
}

/// Try to route weather-related input.
fn try_weather_route(lower: &str, original: &str) -> Option<Route> {
    // Conversational weather → weather-ask
    const CONVERSATIONAL: &[&str] = &[
        "will it rain",
        "is it rain",
        "do i need an umbrella",
        "do i need a jacket",
        "should i wear",
        "is it cold",
        "is it hot",
        "is it warm",
        "is it snowing",
    ];
    for phrase in CONVERSATIONAL {
        if lower.contains(phrase) {
            return Some(Route {
                handler: "weather-ask",
                args: original.to_string(),
                explicit: false,
            });
        }
    }

    // "weather in <city>" / "temperature in <city>" / "forecast for <city>"
    for prefix in &[
        "weather in ",
        "temperature in ",
        "forecast for ",
        "forecast in ",
    ] {
        if lower.starts_with(prefix) {
            let city = original[prefix.len()..].trim();
            if !city.is_empty() {
                return Some(Route {
                    handler: "weather",
                    args: city.to_string(),
                    explicit: false,
                });
            }
        }
    }

    // "what's the weather in <city>" / "how's the weather in <city>"
    if let Some(pos) = lower
        .find("weather in ")
        .or_else(|| lower.find("weather at "))
    {
        let preposition_len = if lower[pos..].starts_with("weather in") {
            "weather in ".len()
        } else {
            "weather at ".len()
        };
        let city = original[pos + preposition_len..].trim();
        if !city.is_empty() {
            return Some(Route {
                handler: "weather",
                args: city.to_string(),
                explicit: false,
            });
        }
    }

    // "what's the weather" (no city) — default location
    if lower.contains("the weather") || lower == "weather" {
        return Some(Route {
            handler: "weather",
            args: String::new(),
            explicit: false,
        });
    }

    None
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
        let r = route("web rust lang");
        assert_eq!(r.handler, "web");
        assert_eq!(r.args, "rust lang");
        assert!(r.explicit);

        let r = route("yt funny cats");
        assert_eq!(r.handler, "yt");
        assert_eq!(r.args, "funny cats");

        let r = route("run ls -la");
        assert_eq!(r.handler, "run");
        assert_eq!(r.args, "ls -la");

        let r = route("open firefox");
        assert_eq!(r.handler, "open");
        assert_eq!(r.args, "firefox");
    }

    #[test]
    fn trigger_characters() {
        let r = route("=2+2");
        assert_eq!(r.handler, "calc");
        assert_eq!(r.args, "2+2");
        assert!(r.explicit);

        let r = route("=sqrt(144)");
        assert_eq!(r.handler, "calc");
        assert_eq!(r.args, "sqrt(144)");

        let r = route(">ls -la");
        assert_eq!(r.handler, "run");
        assert_eq!(r.args, "ls -la");
        assert!(r.explicit);
    }

    #[test]
    fn file_paths() {
        let r = route("~/Documents");
        assert_eq!(r.handler, "file");
        assert_eq!(r.args, "~/Documents");

        let r = route("/tmp");
        assert_eq!(r.handler, "file");

        let r = route("./src");
        assert_eq!(r.handler, "file");
    }

    #[test]
    fn urls() {
        let r = route("https://github.com");
        assert_eq!(r.handler, "url");

        let r = route("github.com");
        assert_eq!(r.handler, "url");

        let r = route("example.org/path");
        assert_eq!(r.handler, "url");

        // Not a URL — has spaces
        let r = route("not a url.com");
        assert_ne!(r.handler, "url");
    }

    #[test]
    fn math_expressions() {
        let r = route("2+2");
        assert_eq!(r.handler, "calc");
        assert_eq!(r.args, "2+2");

        let r = route("100/3");
        assert_eq!(r.handler, "calc");

        let r = route("(10 + 5) * 3");
        assert_eq!(r.handler, "calc");

        // Not math — contains letters
        let r = route("2x+3");
        assert_ne!(r.handler, "calc");
    }

    #[test]
    fn default_app_search() {
        let r = route("firefox");
        assert_eq!(r.handler, "open");
        assert_eq!(r.args, "firefox");
        assert!(!r.explicit);

        let r = route("spotify");
        assert_eq!(r.handler, "spotify");
        assert!(r.explicit);
    }

    #[test]
    fn sysinfo_commands() {
        let r = route("sysinfo");
        assert_eq!(r.handler, "sysinfo");
        assert_eq!(r.args, "");
        assert!(r.explicit);

        let r = route("sysinfo cpu");
        assert_eq!(r.handler, "sysinfo");
        assert_eq!(r.args, "cpu");

        // Bare shortcuts pass keyword as args
        let r = route("ip");
        assert_eq!(r.handler, "sysinfo");
        assert_eq!(r.args, "ip");

        let r = route("cpu");
        assert_eq!(r.handler, "sysinfo");
        assert_eq!(r.args, "cpu");

        let r = route("mem");
        assert_eq!(r.handler, "sysinfo");
        assert_eq!(r.args, "mem");

        let r = route("disk");
        assert_eq!(r.handler, "sysinfo");
        assert_eq!(r.args, "disk");
    }

    // --- Keyword routing tests ---

    #[test]
    fn keyword_system_power() {
        let r = route("shut down the computer");
        assert_eq!(r.handler, "system");
        assert_eq!(r.args, "shutdown");
        assert!(!r.explicit);

        assert_eq!(route("lock my screen").handler, "system");
        assert_eq!(route("lock my screen").args, "lock");

        assert_eq!(route("put the computer to sleep").handler, "system");
        assert_eq!(route("put the computer to sleep").args, "suspend");

        assert_eq!(route("log out").handler, "system");
        assert_eq!(route("log out").args, "logout");

        // "reboot" as bare word → keyword routing
        let r = route("reboot");
        assert_eq!(r.handler, "system");
        assert_eq!(r.args, "reboot");
        assert!(!r.explicit);
    }

    #[test]
    fn keyword_media() {
        let r = route("pause the music");
        assert_eq!(r.handler, "media");
        assert_eq!(r.args, "pause");

        let r = route("skip this song");
        assert_eq!(r.handler, "media");
        assert_eq!(r.args, "next");

        let r = route("pause everything");
        assert_eq!(r.handler, "media");
        assert_eq!(r.args, "pause all");

        let r = route("stop all music");
        assert_eq!(r.handler, "media");
        assert_eq!(r.args, "pause all");
    }

    #[test]
    fn keyword_spotify() {
        let r = route("play something on spotify");
        assert_eq!(r.handler, "spotify");
        assert_eq!(r.args, "play");

        let r = route("next song on spotify");
        assert_eq!(r.handler, "spotify");
        assert_eq!(r.args, "next");
    }

    #[test]
    fn keyword_sysinfo_phrases() {
        let r = route("what's my ip address");
        assert_eq!(r.handler, "sysinfo");
        assert_eq!(r.args, "ip");

        let r = route("how much ram is being used");
        assert_eq!(r.handler, "sysinfo");
        assert_eq!(r.args, "mem");

        let r = route("show cpu usage");
        assert_eq!(r.handler, "sysinfo");
        assert_eq!(r.args, "cpu");

        let r = route("how much disk space is left");
        assert_eq!(r.handler, "sysinfo");
        assert_eq!(r.args, "disk");
    }

    #[test]
    fn keyword_weather() {
        let r = route("weather in tokyo");
        assert_eq!(r.handler, "weather");
        assert_eq!(r.args, "tokyo");

        let r = route("what's the weather in paris");
        assert_eq!(r.handler, "weather");
        assert_eq!(r.args, "paris");

        let r = route("what's the weather");
        assert_eq!(r.handler, "weather");
        assert_eq!(r.args, "");

        // Conversational → weather-ask
        let r = route("will it rain today");
        assert_eq!(r.handler, "weather-ask");

        let r = route("do I need an umbrella");
        assert_eq!(r.handler, "weather-ask");

        let r = route("should I wear a jacket tomorrow");
        assert_eq!(r.handler, "weather-ask");

        let r = route("is it cold outside");
        assert_eq!(r.handler, "weather-ask");
    }

    #[test]
    fn keyword_todo() {
        let r = route("remind me to buy milk");
        assert_eq!(r.handler, "todo");
        assert_eq!(r.args, "add buy milk");

        let r = route("add to my list: fix the login bug");
        assert_eq!(r.handler, "todo");
        assert_eq!(r.args, "add fix the login bug");

        let r = route("what's on my plate");
        assert_eq!(r.handler, "todo");
        assert_eq!(r.args, "summary");

        let r = route("show my todos");
        assert_eq!(r.handler, "todo");
        assert_eq!(r.args, "list");
    }

    #[test]
    fn keyword_notes() {
        let r = route("jot down: call dentist tomorrow");
        assert_eq!(r.handler, "note");
        assert_eq!(r.args, "call dentist tomorrow");

        let r = route("write down meeting at 3pm");
        assert_eq!(r.handler, "note");
        assert_eq!(r.args, "meeting at 3pm");

        let r = route("what did I write down");
        assert_eq!(r.handler, "note");
        assert_eq!(r.args, "read");
    }

    #[test]
    fn keyword_ask() {
        // Only unambiguous starters — "explain" and "define"
        let r = route("explain quantum computing");
        assert_eq!(r.handler, "ask");

        let r = route("define ephemeral");
        assert_eq!(r.handler, "ask");

        // Broad question words fall through to default (AI handles them)
        let r = route("what is the capital of France");
        assert_eq!(r.handler, "open"); // AI will route to "ask"

        let r = route("who invented the telephone");
        assert_eq!(r.handler, "open"); // AI will route to "ask"

        let r = route("how does photosynthesis work");
        assert_eq!(r.handler, "open"); // AI will route to "ask"
    }

    #[test]
    fn keyword_no_false_positives() {
        // Regular app names should not trigger keyword routing
        let r = route("firefox");
        assert_eq!(r.handler, "open");

        // Ambiguous input should fall through to default
        let r = route("find large files in downloads");
        assert_eq!(r.handler, "open"); // Falls to AI in practice

        // Explicit prefix still wins over keyword
        let r = route("weather london");
        assert_eq!(r.handler, "weather");
        assert!(r.explicit);
    }
}

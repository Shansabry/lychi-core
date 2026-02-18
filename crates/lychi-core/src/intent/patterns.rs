//! Smart input router — Alfred-style hybrid routing.
//!
//! Routes user input to the appropriate handler based on:
//! 1. Explicit prefixes (`web `, `yt `, `run `, `open `, `calc `)
//! 2. Trigger characters (`=` for calc, `>` for shell)
//! 3. Pattern detection (file paths, URLs, math expressions)
//! 4. Default: app search → web search fallback

/// Known handler prefixes.
const KNOWN_PREFIXES: &[&str] = &[
    "open", "web", "yt", "run", "calc", "file", "url", "spotify", "project", "system", "note",
    "notes", "todo", "todos",
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

    // 6. Home directory entry (e.g. "Downloads", "Documents", "report.pdf")
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

    // 7. Default — app search
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
        let handler = match lower.as_str() {
            "open" => "open",
            "web" => "web",
            "yt" => "yt",
            "run" => "run",
            "calc" => "calc",
            "file" => "file",
            "url" => "url",
            "spotify" => "spotify",
            "project" => "project",
            "system" => "system",
            "note" | "notes" => "note",
            "todo" | "todos" => "todo",
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
}

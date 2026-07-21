use crate::action_registry::RiskLevel;
use crate::error::LychiError;
use crate::providers::{AgentPlan, AgentStep, AiResponse, AiRoute, generate_plan_id};
use crate::rules::shell::ShellRules;

/// Bump this when the system prompt changes in a semantically meaningful way.
/// Included in debug logs so bad routes can be tied to the prompt that produced them.
pub const PROMPT_VERSION: &str = "v5";

/// Build the system prompt for intent routing.
///
/// If `context_hint` is provided, it's appended to help the AI make
/// context-aware decisions (e.g., knowing the user is in a Rust project).
pub fn system_prompt(known_actions: &[&str], context_hint: Option<&str>) -> String {
    let commands = known_actions
        .iter()
        .map(|cmd| {
            let desc = action_description(cmd);
            format!("- {cmd} <args>: {desc}")
        })
        .collect::<Vec<_>>()
        .join("\n");

    let context_section = context_hint
        .map(|hint| format!("\n\nUser's current context:\n{hint}"))
        .unwrap_or_default();

    format!(
        r#"You are a command router for a desktop launcher. Given natural language input, determine the best matching command and extract the arguments.

Most common inputs (weather, system power, media control, sysinfo, todos, notes, timers, direct questions) are already handled before reaching you. You only see ambiguous or complex cases.

Available commands:
{commands}

Response format:
{{"action_id": "...", "args": "..."}}

Examples (non-obvious argument transformations only):
- "find agent agnes files in downloads" → {{"action_id": "run", "args": "ls ~/Downloads/*gent*gnes*"}}
- "how much is 15% of 200" → {{"action_id": "calc", "args": "200 * 0.15"}}
- "open readyroos in vscode" → {{"action_id": "project", "args": "readyroos"}}
- "open github.com" → {{"action_id": "url", "args": "https://github.com"}}
- "make hello world uppercase" → {{"action_id": "devutil", "args": "upper hello world"}}
- "format this json {{\"a\":1}}" → {{"action_id": "devutil", "args": "json {{\"a\":1}}"}}
- "slugify My Blog Post" → {{"action_id": "devutil", "args": "slug My Blog Post"}}
- "random number between 1 and 100" → {{"action_id": "generate", "args": "random 1 100"}}
- "roll a dice" → {{"action_id": "generate", "args": "random 1 6"}}
- "take a screenshot of a region" → {{"action_id": "screenshot", "args": "area"}}
- "capture the active window" → {{"action_id": "screenshot", "args": "window"}}
- "restart nginx" → {{"action_id": "service", "args": "nginx restart"}}
- "is docker running" → {{"action_id": "service", "args": "docker status"}}
- "install neovim" → {{"action_id": "packages", "args": "install neovim"}}
- "search for a markdown editor package" → {{"action_id": "packages", "args": "search markdown editor"}}

Rules:
- "open" = GUI apps by name. "project" = open a project folder in editor. "browse" = browse a directory. "run" = shell commands.
- For file searches by name/type, use "run" with ls or find — not "browse".
- When uncertain between two handlers, prefer the more specific one.
- Extract clean arguments — strip filler words ("please", "can you", "I want to").
- Respond with ONLY valid JSON. No extra text.
- Fallback: question → "ask", non-question → "web".{context_section}

<!-- prompt_version={PROMPT_VERSION} -->"#
    )
}

fn action_description(id: &str) -> &'static str {
    match id {
        "ask" => {
            "Ask a question and get an AI-powered inline answer. Use for direct questions (what, who, how, why, explain, define). Prefer over 'web' when the user is asking a question"
        }
        "open" => "Launch a desktop application by name (e.g. firefox, spotify, vscode)",
        "web" => "Search the web for general lookups or non-question searches",
        "yt" => "Search YouTube for videos",
        "run" => "Execute a shell command (e.g. 'code .', 'ls ~', 'htop')",
        "calc" => "Evaluate a math expression (e.g. '2+2', 'sqrt(144)')",
        "browse" => {
            "Browse a directory interactively. ONLY use when the user wants to open/browse a whole folder without filtering (e.g. 'browse downloads', 'show my documents folder'). If the user mentions specific filenames or search terms, use 'run' with ls/find instead"
        }
        "file" => "Open a file or directory in the default app (e.g. '~/Downloads')",
        "url" => "Open a URL in the browser (e.g. 'https://github.com')",
        "media" => {
            "Control media players. Args: play, pause, next, prev, toggle, 'pause all'. Prefix with provider to target a specific player (e.g. 'spotify pause', 'yt next'). Use for any media/music control"
        }
        "project" => {
            "Open a project folder by name in the code editor (e.g. 'readyroos', 'lychi'). Use when the user wants to open a project in VSCode/editor by its name"
        }
        "sysinfo" => {
            "System info — show IP address, CPU, memory, or disk usage. Subcommands: ip, cpu, mem, disk. Empty args shows a full overview"
        }
        "system" => {
            "System controls. Args: shutdown, reboot, suspend, hibernate, lock, logout, mute, unmute, volume <up|down|0-100>, brightness <up|down|0-100>, wifi <on|off>, bluetooth <on|off>, connect bluetooth <device>, disconnect bluetooth <device>, shutdown in <duration> (e.g. 'shutdown in 30m'), cancel shutdown"
        }
        "note" => {
            "Notes list (max 5). Args: text to add a note, 'read' to view all, 'delete <id>' to remove"
        }
        "todo" => "Todo list. Args: 'add <text>', 'list', 'done <id>', 'delete <id>', 'summary'",
        "weather" => "Get current weather/forecast. Args: city name, or empty for default location",
        "weather-ask" => {
            "Answer conversational weather questions with real data. Args: the full question (e.g. 'will it rain today', 'do I need a jacket')"
        }
        "timer" => {
            "Timer and stopwatch. Args: 'start [name] <duration>' (e.g. 'start 25m', 'start workout 5m'), 'stopwatch [name]' to start a count-up stopwatch, 'stop [name]', 'pause [name]', 'resume [name]', 'status', 'clear'. Shorthand: bare duration like '25m' starts a timer"
        }
        "time" => {
            "Show current time in a timezone or convert between timezones. Args: timezone name or city (e.g. 'tokyo', 'EST', 'london'). Empty for local time"
        }
        "alias" => {
            "Manage command aliases. Args: 'set <name> <command>', 'remove <name>', 'list'. Creates shortcuts for common commands"
        }
        "reminder" => {
            "Set timed reminders with desktop notifications. Args: 'add <text> in/at <time>' (e.g. 'add buy milk in 30m', 'add standup at 9am', 'add meeting tomorrow 2pm'), 'list', 'delete <id>', 'clear'. Without 'add', infers from natural language"
        }
        "appctl" => "Focus, quit, or kill a running application by name",
        "bm" => "Search and open browser bookmarks by keyword",
        "clip" => "Browse and paste from clipboard history",
        "emoji" => "Search and copy an emoji by name or keyword",
        "snip" => "Snippets — save and paste reusable text blocks",
        "sym" => "Search and copy a symbol or special character by name",
        "unicode" => "Search Unicode characters by name or codepoint",
        "devutil" => {
            "Developer text utilities. Prepend the verb to the text. Verbs: 'base64 <text>' / 'base64 -d <b64>', 'hash [md5|sha256] <text>', 'urlencode <text>' / 'urldecode <text>', 'epoch [<unix-seconds>]', 'json <text>' (pretty-print) / 'json -m <text>' (minify), 'upper <text>' / 'lower <text>' / 'title <text>' (change case), 'slug <text>' (url-safe slug), 'reverse <text>', 'count <text>' (chars/words/lines). Use for 'encode/decode', 'make uppercase', 'format this json', 'slugify', etc."
        }
        "generate" => {
            "Generate random values. Args: 'password [length]', 'uuid', 'token [length]', 'random [min] <max>' (random integer, default 0–100). Use for 'generate a password', 'give me a uuid', 'random number between 1 and 100', 'roll a dice'"
        }
        "color" => "Convert or inspect a color between HEX, RGB, and HSL (e.g. '#ff5733')",
        "screenshot" => {
            "Take a screenshot. Args: empty or 'full' for the whole screen, 'area' (aliases: region, select) to select a region, 'window' for the active window. Use for 'take a screenshot', 'capture this region', 'grab a screenshot of the window'"
        }
        "services" => "List currently running systemd services",
        "service" => {
            "Control a systemd service. Args: '<name>' or '<name> status' to check it; '<name> start|stop|restart|reload|enable|disable' to control it. Use for 'restart nginx', 'is docker running', 'stop the bluetooth service'"
        }
        "packages" => {
            "Search or install SYSTEM packages via the OS package manager (dnf/apt/pacman/flatpak). Args: 'search <query>' or 'install <package>'. Use for 'install neovim', 'search for a markdown editor', 'is ripgrep available to install'. NOT for web searches"
        }
        _ => "Unknown command",
    }
}

/// Strip markdown code fences from AI response.
fn strip_code_fences(response: &str) -> &str {
    let trimmed = response.trim();
    let stripped = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .unwrap_or(trimmed);
    stripped.strip_suffix("```").unwrap_or(stripped).trim()
}

/// Extract the first balanced top-level JSON object from a string, tolerating
/// leading/trailing prose. Small local models frequently append a stray token or
/// a sentence after the JSON (`{...} </s>`, `{...} Let me know...`), which a
/// strict whole-string parse rejects. We scan for the first `{`, then track brace
/// depth (ignoring braces inside strings / escapes) to find its matching `}`.
/// Returns the object slice, or the input unchanged if no object is found (so the
/// caller's parse produces the original, meaningful error).
fn extract_json_object(s: &str) -> &str {
    let bytes = s.as_bytes();
    let Some(start) = bytes.iter().position(|&b| b == b'{') else {
        return s;
    };
    let mut depth = 0usize;
    let mut in_str = false;
    let mut escaped = false;
    for (i, &b) in bytes.iter().enumerate().skip(start) {
        if in_str {
            match b {
                _ if escaped => escaped = false,
                b'\\' => escaped = true,
                b'"' => in_str = false,
                _ => {}
            }
            continue;
        }
        match b {
            b'"' => in_str = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return &s[start..=i];
                }
            }
            _ => {}
        }
    }
    s
}

/// Validate and potentially upgrade the risk level of a shell step.
fn validate_step_risk(step: &AgentStep) -> RiskLevel {
    if step.action_id == "run" {
        let shell_rules = ShellRules::new();
        match shell_rules.validate(&step.args) {
            crate::rules::ValidationDecision::Deny { .. } => RiskLevel::High,
            crate::rules::ValidationDecision::Confirm { .. } => {
                // At least Medium, but keep High if AI said High
                if step.risk >= RiskLevel::Medium {
                    step.risk
                } else {
                    RiskLevel::Medium
                }
            }
            crate::rules::ValidationDecision::Execute => step.risk,
        }
    } else {
        step.risk
    }
}

fn json_type_name(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Bool(_) => "bool",
        serde_json::Value::Null => "null",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Object(_) => "object",
    }
}

/// Parse an AI response into either a single route or a multi-step plan.
pub fn parse_ai_response(
    response: &str,
    known_actions: &[&str],
    original_input: &str,
) -> Result<AiResponse, LychiError> {
    // Strip markdown fences, then isolate the first balanced JSON object so a
    // trailing token/sentence from a small local model doesn't fail the parse.
    let cleaned = extract_json_object(strip_code_fences(response));

    let json: serde_json::Value = serde_json::from_str(cleaned)
        .map_err(|e| LychiError::Ai(format!("Failed to parse AI response as JSON: {e}")))?;

    // Response must be a JSON object — arrays and primitives are invalid
    if !json.is_object() {
        return Err(LychiError::Ai(format!(
            "AI response must be a JSON object, got: {}",
            json_type_name(&json)
        )));
    }

    // Check if response has "steps" array (multi-step plan)
    if let Some(steps_val) = json.get("steps") {
        let raw_steps: Vec<AgentStep> = serde_json::from_value(steps_val.clone())
            .map_err(|e| LychiError::Ai(format!("Failed to parse steps array: {e}")))?;

        if raw_steps.is_empty() {
            return Err(LychiError::Ai("Empty steps array".to_string()));
        }

        // Single-step plan → collapse to single route
        if raw_steps.len() == 1 {
            let step = &raw_steps[0];
            if !known_actions.contains(&step.action_id.as_str()) {
                return Err(LychiError::Ai(format!(
                    "AI returned unknown command: {}",
                    step.action_id
                )));
            }
            return Ok(AiResponse::SingleRoute(AiRoute {
                action_id: step.action_id.clone(),
                args: step.args.clone(),
            }));
        }

        // Validate all commands are known and apply risk validation
        let mut steps = Vec::with_capacity(raw_steps.len());
        for mut step in raw_steps {
            if !known_actions.contains(&step.action_id.as_str()) {
                return Err(LychiError::Ai(format!(
                    "AI returned unknown command in step: {}",
                    step.action_id
                )));
            }
            step.risk = validate_step_risk(&step);
            steps.push(step);
        }

        return Ok(AiResponse::Plan(AgentPlan {
            id: generate_plan_id(),
            input: original_input.to_string(),
            steps,
        }));
    }

    // Single route (existing format)
    let route: AiRoute = serde_json::from_value(json)
        .map_err(|e| LychiError::Ai(format!("Failed to parse AI route: {e}")))?;

    if !known_actions.contains(&route.action_id.as_str()) {
        return Err(LychiError::Ai(format!(
            "AI returned unknown command: {}",
            route.action_id
        )));
    }

    Ok(AiResponse::SingleRoute(route))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_single_route() {
        let known = &["web", "yt", "open", "run"];
        let resp = parse_ai_response(
            r#"{"action_id": "web", "args": "rust tutorials"}"#,
            known,
            "rust tutorials",
        )
        .unwrap();
        match resp {
            AiResponse::SingleRoute(route) => {
                assert_eq!(route.action_id, "web");
                assert_eq!(route.args, "rust tutorials");
            }
            AiResponse::Plan(_) => panic!("Expected SingleRoute"),
        }
    }

    #[test]
    fn parse_with_code_fences() {
        let known = &["yt"];
        let resp = parse_ai_response(
            "```json\n{\"action_id\": \"yt\", \"args\": \"lofi\"}\n```",
            known,
            "play lofi",
        )
        .unwrap();
        match resp {
            AiResponse::SingleRoute(route) => {
                assert_eq!(route.action_id, "yt");
                assert_eq!(route.args, "lofi");
            }
            AiResponse::Plan(_) => panic!("Expected SingleRoute"),
        }
    }

    #[test]
    fn parse_multi_step_plan() {
        let known = &["run", "open"];
        let resp = parse_ai_response(
            r#"{"steps": [
                {"action_id": "run", "args": "cargo init my-project", "label": "Create project", "risk": "medium"},
                {"action_id": "run", "args": "code my-project", "label": "Open in VS Code", "risk": "low"}
            ]}"#,
            known,
            "create rust project and open in vscode",
        )
        .unwrap();
        match resp {
            AiResponse::Plan(plan) => {
                assert_eq!(plan.steps.len(), 2);
                assert_eq!(plan.steps[0].action_id, "run");
                assert_eq!(plan.steps[0].risk, RiskLevel::Medium);
                assert_eq!(plan.steps[1].action_id, "run");
            }
            AiResponse::SingleRoute(_) => panic!("Expected Plan"),
        }
    }

    #[test]
    fn single_step_plan_collapses_to_route() {
        let known = &["run"];
        let resp = parse_ai_response(
            r#"{"steps": [{"action_id": "run", "args": "code .", "label": "Open VS Code", "risk": "low"}]}"#,
            known,
            "open vscode",
        )
        .unwrap();
        match resp {
            AiResponse::SingleRoute(route) => {
                assert_eq!(route.action_id, "run");
                assert_eq!(route.args, "code .");
            }
            AiResponse::Plan(_) => panic!("Expected SingleRoute (collapsed)"),
        }
    }

    #[test]
    fn parse_unknown_command() {
        let known = &["web", "yt"];
        let result = parse_ai_response(
            r#"{"action_id": "delete", "args": "everything"}"#,
            known,
            "test",
        );
        assert!(result.is_err());
    }

    #[test]
    fn parse_invalid_json() {
        let known = &["web"];
        let result = parse_ai_response("not json at all", known, "test");
        assert!(result.is_err());
    }

    #[test]
    fn risk_validation_upgrades_in_plan() {
        let known = &["run"];
        let resp = parse_ai_response(
            r#"{"steps": [
                {"action_id": "run", "args": "rm -rf /tmp/foo", "label": "Clean up", "risk": "low"},
                {"action_id": "run", "args": "ls", "label": "List files", "risk": "low"}
            ]}"#,
            known,
            "clean up and list files",
        )
        .unwrap();
        match resp {
            AiResponse::Plan(plan) => {
                // rm -rf upgraded from low → at least medium
                assert!(plan.steps[0].risk >= RiskLevel::Medium);
                assert_eq!(plan.steps[1].risk, RiskLevel::Low); // stays low
            }
            _ => panic!("Expected Plan"),
        }
    }

    #[test]
    fn system_prompt_contains_commands() {
        let prompt = system_prompt(&["web", "yt", "open"], None);
        assert!(prompt.contains("web <args>"));
        assert!(prompt.contains("yt <args>"));
        assert!(prompt.contains("open <args>"));
    }

    #[test]
    fn system_prompt_with_context() {
        let prompt = system_prompt(
            &["run"],
            Some(
                "- Working directory: ~/projects/lychi\n- Git branch: main (dirty)\n- Project type: Rust",
            ),
        );
        assert!(prompt.contains("User's current context:"));
        assert!(prompt.contains("Git branch: main"));
        assert!(prompt.contains("Rust"));
    }

    #[test]
    fn system_prompt_contains_version() {
        let prompt = system_prompt(&["web"], None);
        assert!(
            prompt.contains(PROMPT_VERSION),
            "system prompt must embed PROMPT_VERSION"
        );
    }

    // --- Adversarial response tests ---

    #[test]
    fn adversarial_empty_string() {
        let known = &["web", "run"];
        assert!(parse_ai_response("", known, "test").is_err());
    }

    #[test]
    fn adversarial_whitespace_only() {
        let known = &["web"];
        assert!(parse_ai_response("   \n\t  ", known, "test").is_err());
    }

    #[test]
    fn adversarial_plain_text_no_json() {
        let known = &["web", "run"];
        assert!(parse_ai_response("Sure! I'll open Firefox for you.", known, "test").is_err());
    }

    #[test]
    fn adversarial_truncated_json() {
        let known = &["web"];
        assert!(parse_ai_response(r#"{"action_id": "web", "args":"#, known, "test").is_err());
    }

    #[test]
    fn tolerates_trailing_text_after_json() {
        // Small local models often append a stray token/sentence after the JSON.
        // The first balanced object must still parse (extract_json_object).
        let known = &["web"];
        let r = parse_ai_response(
            r#"{"action_id": "web", "args": "rust"} </s> hope that helps!"#,
            known,
            "test",
        );
        assert!(r.is_ok(), "trailing text after the JSON object must be tolerated");
    }

    #[test]
    fn tolerates_leading_prose_before_json() {
        let known = &["web"];
        let r = parse_ai_response(
            r#"Sure! Here is the route: {"action_id": "web", "args": "rust"}"#,
            known,
            "test",
        );
        assert!(r.is_ok(), "leading prose before the JSON object must be tolerated");
    }

    #[test]
    fn extract_json_object_handles_nested_and_strings() {
        // Braces inside strings must not confuse the balance scan.
        let s = r#"junk {"a": {"b": "text with } brace"}, "c": 1} tail"#;
        assert_eq!(
            extract_json_object(s),
            r#"{"a": {"b": "text with } brace"}, "c": 1}"#
        );
    }

    #[test]
    fn adversarial_empty_action_id() {
        let known = &["web", "run"];
        // Empty string is not in known_actions → should be rejected
        let result = parse_ai_response(r#"{"action_id": "", "args": "foo"}"#, known, "test");
        assert!(result.is_err(), "empty action_id must be rejected");
    }

    #[test]
    fn adversarial_unknown_action_id() {
        let known = &["web", "run"];
        let result = parse_ai_response(
            r#"{"action_id": "rm_everything", "args": "-rf /"}"#,
            known,
            "test",
        );
        assert!(result.is_err(), "unknown action_id must be rejected");
    }

    #[test]
    fn adversarial_missing_action_id_field() {
        let known = &["web"];
        // Valid JSON but missing action_id field — serde should fail
        let result = parse_ai_response(r#"{"args": "foo"}"#, known, "test");
        assert!(result.is_err(), "missing action_id field must be rejected");
    }

    #[test]
    fn adversarial_null_fields() {
        let known = &["web"];
        let result = parse_ai_response(r#"{"action_id": null, "args": null}"#, known, "test");
        assert!(result.is_err(), "null action_id must be rejected");
    }

    #[test]
    fn adversarial_oversized_args() {
        // 10KB args string — parse should succeed (we don't truncate at parse time)
        // but the action_id must still be valid
        let known = &["run"];
        let big_args = "x".repeat(10_000);
        let json = format!(r#"{{"action_id": "run", "args": "{big_args}"}}"#);
        let result = parse_ai_response(&json, known, "test");
        // Oversized args: parse succeeds but callers should apply their own limits
        assert!(result.is_ok(), "oversized args should parse without panic");
        if let Ok(AiResponse::SingleRoute(route)) = result {
            assert_eq!(route.args.len(), 10_000);
        }
    }

    #[test]
    fn adversarial_prompt_injection_in_args() {
        // Prompt injection attempt in args field — parse_ai_response only validates
        // action_id against known list; the args value is passed through as-is.
        // This test documents current behavior: args are NOT sanitized here
        // (sanitization is the rules engine's job).
        let known = &["run"];
        let injection = r#"{"action_id": "run", "args": "ls; rm -rf ~"}"#;
        let result = parse_ai_response(injection, known, "test");
        assert!(
            result.is_ok(),
            "injection in args parses (rules engine sanitizes later)"
        );
        if let Ok(AiResponse::SingleRoute(route)) = result {
            assert!(
                route.args.contains("rm -rf"),
                "args pass through unmodified"
            );
        }
    }

    #[test]
    fn adversarial_prompt_injection_in_action_id() {
        // Injection attempt in action_id itself — must be rejected as unknown
        let known = &["run", "web"];
        let result = parse_ai_response(
            r#"{"action_id": "run\"; system(\"reboot", "args": ""}"#,
            known,
            "test",
        );
        assert!(
            result.is_err(),
            "injected action_id must be rejected as unknown"
        );
    }

    #[test]
    fn adversarial_array_instead_of_object() {
        let known = &["web"];
        assert!(parse_ai_response(r#"["web", "rust"]"#, known, "test").is_err());
    }

    #[test]
    fn adversarial_nested_object_in_action_id() {
        let known = &["web"];
        let result = parse_ai_response(
            r#"{"action_id": {"nested": "web"}, "args": "foo"}"#,
            known,
            "test",
        );
        assert!(result.is_err(), "object in action_id must be rejected");
    }

    #[test]
    fn adversarial_empty_steps_array() {
        let known = &["run"];
        let result = parse_ai_response(r#"{"steps": []}"#, known, "test");
        assert!(result.is_err(), "empty steps array must be rejected");
    }

    #[test]
    fn adversarial_steps_with_unknown_action() {
        let known = &["run"];
        let result = parse_ai_response(
            r#"{"steps": [
                {"action_id": "run", "args": "ls", "label": "List", "risk": "low"},
                {"action_id": "delete_system", "args": "/", "label": "Nuke", "risk": "low"}
            ]}"#,
            known,
            "test",
        );
        assert!(
            result.is_err(),
            "unknown action in steps must reject entire plan"
        );
    }
}

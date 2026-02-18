use crate::action_registry::RiskLevel;
use crate::error::LychiError;
use crate::providers::{AgentPlan, AgentStep, AiResponse, AiRoute, generate_plan_id};
use crate::rules::shell::ShellRules;

/// Build the system prompt for intent routing.
pub fn system_prompt(known_actions: &[&str]) -> String {
    let commands = known_actions
        .iter()
        .map(|cmd| {
            let desc = action_description(cmd);
            format!("- {cmd} <args>: {desc}")
        })
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        r#"You are a command router for a desktop launcher on Linux. Given natural language input, determine the best matching command and extract the arguments.

Available commands:
{commands}

For simple requests (one action), respond with:
{{"action_id": "...", "args": "..."}}

For complex requests (2+ distinct actions), respond with a steps array:
{{"steps": [
  {{"action_id": "run", "args": "cargo init my-project", "label": "Create Rust project", "risk": "medium"}},
  {{"action_id": "run", "args": "code my-project", "label": "Open in VS Code", "risk": "low"}}
]}}

Risk levels per step:
- "low": read-only or standard operations (ls, cat, code, open, firefox)
- "medium": creates or modifies files (mkdir, touch, cargo init, npm init)
- "high": destructive or system operations (rm, sudo, chmod, operations with > or |)

Examples:
- "open firefox" → {{"action_id": "open", "args": "firefox"}}
- "play lofi on youtube" → {{"action_id": "yt", "args": "lofi"}}
- "what's the weather" → {{"action_id": "weather", "args": ""}}
- "weather in paris" → {{"action_id": "weather", "args": "paris"}}
- "is it raining in tokyo" → {{"action_id": "weather", "args": "tokyo"}}
- "temperature in new york" → {{"action_id": "weather", "args": "new york"}}
- "will it rain today" → {{"action_id": "weather-ask", "args": "will it rain today"}}
- "do I need an umbrella" → {{"action_id": "weather-ask", "args": "do I need an umbrella"}}
- "is it cold outside" → {{"action_id": "weather-ask", "args": "is it cold outside"}}
- "should I wear a jacket tomorrow" → {{"action_id": "weather-ask", "args": "should I wear a jacket tomorrow"}}
- "open this folder in vscode" → {{"action_id": "run", "args": "code ."}}
- "browse downloads" → {{"action_id": "browse", "args": "~/Downloads"}}
- "show my documents folder" → {{"action_id": "browse", "args": "~/Documents"}}
- "find agent agnes files in downloads" → {{"action_id": "run", "args": "ls ~/Downloads/*gent*gnes*"}}
- "list pdf files in documents" → {{"action_id": "run", "args": "ls ~/Documents/*.pdf"}}
- "how much is 15% of 200" → {{"action_id": "calc", "args": "200 * 0.15"}}
- "open github.com" → {{"action_id": "url", "args": "https://github.com"}}
- "open downloads folder" → {{"action_id": "file", "args": "~/Downloads"}}
- "skip this song" → {{"action_id": "media", "args": "next"}}
- "pause the music" → {{"action_id": "media", "args": "pause"}}
- "play something on spotify" → {{"action_id": "spotify", "args": "play"}}
- "pause chrome media" → {{"action_id": "media", "args": "pause"}}
- "next song on spotify" → {{"action_id": "spotify", "args": "next"}}
- "pause everything" → {{"action_id": "media", "args": "pause all"}}
- "stop all music" → {{"action_id": "media", "args": "pause all"}}
- "shut down the computer" → {{"action_id": "system", "args": "shutdown"}}
- "lock my screen" → {{"action_id": "system", "args": "lock"}}
- "reboot" → {{"action_id": "system", "args": "reboot"}}
- "put the computer to sleep" → {{"action_id": "system", "args": "suspend"}}
- "remind me to buy milk" → {{"action_id": "todo", "args": "add buy milk"}}
- "add to my list: fix the login bug" → {{"action_id": "todo", "args": "add fix the login bug"}}
- "what's on my plate" → {{"action_id": "todo", "args": "summary"}}
- "what's left to do" → {{"action_id": "todo", "args": "summary"}}
- "show my todos" → {{"action_id": "todo", "args": "list"}}
- "did I forget something" → {{"action_id": "todo", "args": "summary"}}
- "jot down: call dentist tomorrow" → {{"action_id": "note", "args": "call dentist tomorrow"}}
- "what did I write down" → {{"action_id": "note", "args": "read"}}
- "read my note" → {{"action_id": "note", "args": "read"}}
- "what is the capital of France" → {{"action_id": "ask", "args": "what is the capital of France"}}
- "who invented the telephone" → {{"action_id": "ask", "args": "who invented the telephone"}}
- "explain quantum computing" → {{"action_id": "ask", "args": "explain quantum computing"}}
- "how does photosynthesis work" → {{"action_id": "ask", "args": "how does photosynthesis work"}}
- "open readyroos in vscode" → {{"action_id": "project", "args": "readyroos"}}
- "open my lychi project" → {{"action_id": "project", "args": "lychi"}}
- "create a new rust project and open in vscode" → {{"steps": [{{"action_id": "run", "args": "cargo init my-project", "label": "Create Rust project", "risk": "medium"}}, {{"action_id": "run", "args": "code my-project", "label": "Open in VS Code", "risk": "low"}}]}}
- "start a node project, install express, and open in code" → {{"steps": [{{"action_id": "run", "args": "mkdir my-app && cd my-app && npm init -y", "label": "Create Node project", "risk": "medium"}}, {{"action_id": "run", "args": "cd my-app && npm install express", "label": "Install Express", "risk": "medium"}}, {{"action_id": "run", "args": "code my-app", "label": "Open in VS Code", "risk": "low"}}]}}

Rules:
- For launching GUI apps by name, use "open".
- For opening a project by name in an editor/IDE, use "project".
- For browsing a whole directory (no search/filter), use "browse". For searching specific files by name/type, use "run" with ls or find.
- For running CLI tools or apps with specific arguments/paths, use "run".
- For direct questions (what, who, how, why, explain, define), use "ask".
- For viewing weather data/forecast (e.g. "weather in london", "temperature in paris"), use "weather".
- For conversational weather questions (e.g. "will it rain today", "do I need an umbrella", "is it cold outside"), use "weather-ask".
- For general searches or non-question lookups (e.g. "news", "reddit"), use "web".
- Only use steps array when 2+ distinct operations are needed.
- Labels should be short (3-5 words).
- Order steps logically (create before open, install before run).
- Extract clean arguments — strip filler words like "please", "can you", "I want to".
- Respond with ONLY valid JSON. No extra text.
- If truly unclear and it looks like a question, use: {{"action_id": "ask", "args": "<original input>"}}
- If truly unclear and it doesn't look like a question, use: {{"action_id": "web", "args": "<original input>"}}"#
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
        "spotify" => {
            "Control Spotify specifically (e.g. 'play', 'pause', 'next', 'prev'). Only use when the user explicitly mentions Spotify"
        }
        "media" => {
            "Control any/all media players (e.g. 'play', 'pause', 'next', 'prev', 'pause all'). Use for generic media commands or when a non-Spotify player is mentioned (Chrome, Firefox, browser, YouTube, VLC, etc.). 'pause all' pauses every running player"
        }
        "project" => {
            "Open a project folder by name in the code editor (e.g. 'readyroos', 'lychi'). Use when the user wants to open a project in VSCode/editor by its name"
        }
        "system" => {
            "System power controls: shutdown, reboot, suspend, hibernate, lock, logout. Use when the user wants to power off, restart, sleep, lock screen, or log out"
        }
        "note" => {
            "Quick sticky note. 'note <text>' to save, 'note read' to view current note. Use when the user wants to jot something down or read their note"
        }
        "todo" => {
            "Todo list. 'todo add <text>' to add, 'todo list' to view all, 'todo done <id>' to check off, 'todo delete <id>' to remove, 'todo summary' for a full overview of note + todos. Use for task management, reminders, or when asking what's on their plate"
        }
        "weather" => {
            "Get current weather for a location (e.g. 'london', 'tokyo'). Use when the user wants to VIEW weather data or forecast"
        }
        "weather-ask" => {
            "Answer conversational weather questions using real weather data (e.g. 'will it rain', 'do I need a jacket', 'is it cold today'). Use for weather QUESTIONS, not for viewing weather data"
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

/// Parse an AI response into either a single route or a multi-step plan.
pub fn parse_ai_response(
    response: &str,
    known_actions: &[&str],
    original_input: &str,
) -> Result<AiResponse, LychiError> {
    let cleaned = strip_code_fences(response);

    let json: serde_json::Value = serde_json::from_str(cleaned)
        .map_err(|e| LychiError::Ai(format!("Failed to parse AI response as JSON: {e}")))?;

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
        let prompt = system_prompt(&["web", "yt", "open"]);
        assert!(prompt.contains("web <args>"));
        assert!(prompt.contains("yt <args>"));
        assert!(prompt.contains("open <args>"));
    }
}

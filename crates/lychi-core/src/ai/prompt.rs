use crate::error::LychiError;

use super::agent::{AgentPlan, AgentStep, AiResponse, generate_plan_id, validate_risk};
use super::provider::AiRoute;

/// Build the system prompt for intent routing.
pub fn system_prompt(known_commands: &[&str]) -> String {
    let commands = known_commands
        .iter()
        .map(|cmd| {
            let desc = command_description(cmd);
            format!("- {cmd} <args>: {desc}")
        })
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        r#"You are a command router for a desktop launcher on Linux. Given natural language input, determine the best matching command and extract the arguments.

Available commands:
{commands}

For simple requests (one action), respond with:
{{"command": "...", "args": "..."}}

For complex requests (2+ distinct actions), respond with a steps array:
{{"steps": [
  {{"command": "run", "args": "cargo init my-project", "label": "Create Rust project", "risk": "moderate"}},
  {{"command": "run", "args": "code my-project", "label": "Open in VS Code", "risk": "safe"}}
]}}

Risk levels per step:
- "safe": read-only or standard operations (ls, cat, code, open, firefox)
- "moderate": creates or modifies files (mkdir, touch, cargo init, npm init)
- "dangerous": destructive or system operations (rm, sudo, chmod, operations with > or |)

Examples:
- "open firefox" → {{"command": "open", "args": "firefox"}}
- "play lofi on youtube" → {{"command": "yt", "args": "lofi"}}
- "what's the weather" → {{"command": "web", "args": "weather"}}
- "open this folder in vscode" → {{"command": "run", "args": "code ."}}
- "list files in home" → {{"command": "run", "args": "ls ~"}}
- "how much is 15% of 200" → {{"command": "calc", "args": "200 * 0.15"}}
- "open github.com" → {{"command": "url", "args": "https://github.com"}}
- "open downloads folder" → {{"command": "file", "args": "~/Downloads"}}
- "skip this song" → {{"command": "media", "args": "next"}}
- "pause the music" → {{"command": "media", "args": "pause"}}
- "play something on spotify" → {{"command": "spotify", "args": "play"}}
- "pause chrome media" → {{"command": "media", "args": "pause"}}
- "next song on spotify" → {{"command": "spotify", "args": "next"}}
- "pause everything" → {{"command": "media", "args": "pause all"}}
- "stop all music" → {{"command": "media", "args": "pause all"}}
- "shut down the computer" → {{"command": "system", "args": "shutdown"}}
- "lock my screen" → {{"command": "system", "args": "lock"}}
- "reboot" → {{"command": "system", "args": "reboot"}}
- "put the computer to sleep" → {{"command": "system", "args": "suspend"}}
- "open readyroos in vscode" → {{"command": "project", "args": "readyroos"}}
- "open my lychi project" → {{"command": "project", "args": "lychi"}}
- "create a new rust project and open in vscode" → {{"steps": [{{"command": "run", "args": "cargo init my-project", "label": "Create Rust project", "risk": "moderate"}}, {{"command": "run", "args": "code my-project", "label": "Open in VS Code", "risk": "safe"}}]}}
- "start a node project, install express, and open in code" → {{"steps": [{{"command": "run", "args": "mkdir my-app && cd my-app && npm init -y", "label": "Create Node project", "risk": "moderate"}}, {{"command": "run", "args": "cd my-app && npm install express", "label": "Install Express", "risk": "moderate"}}, {{"command": "run", "args": "code my-app", "label": "Open in VS Code", "risk": "safe"}}]}}

Rules:
- For launching GUI apps by name, use "open".
- For opening a project by name in an editor/IDE, use "project".
- For running CLI tools or apps with specific arguments/paths, use "run".
- For questions or lookups, use "web".
- Only use steps array when 2+ distinct operations are needed.
- Labels should be short (3-5 words).
- Order steps logically (create before open, install before run).
- Extract clean arguments — strip filler words like "please", "can you", "I want to".
- Respond with ONLY valid JSON. No extra text.
- If truly unclear, use: {{"command": "web", "args": "<original input>"}}"#
    )
}

fn command_description(prefix: &str) -> &'static str {
    match prefix {
        "open" => "Launch a desktop application by name (e.g. firefox, spotify, vscode)",
        "web" => "Search the web for information or questions",
        "yt" => "Search YouTube for videos",
        "run" => "Execute a shell command (e.g. 'code .', 'ls ~', 'htop')",
        "calc" => "Evaluate a math expression (e.g. '2+2', 'sqrt(144)')",
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

/// Parse an AI response into either a single route or a multi-step plan.
pub fn parse_ai_response(
    response: &str,
    known_commands: &[&str],
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
            if !known_commands.contains(&step.command.as_str()) {
                return Err(LychiError::Ai(format!(
                    "AI returned unknown command: {}",
                    step.command
                )));
            }
            return Ok(AiResponse::SingleRoute(AiRoute {
                command: step.command.clone(),
                args: step.args.clone(),
            }));
        }

        // Validate all commands are known and apply risk validation
        let mut steps = Vec::with_capacity(raw_steps.len());
        for mut step in raw_steps {
            if !known_commands.contains(&step.command.as_str()) {
                return Err(LychiError::Ai(format!(
                    "AI returned unknown command in step: {}",
                    step.command
                )));
            }
            step.risk = validate_risk(&step);
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

    if !known_commands.contains(&route.command.as_str()) {
        return Err(LychiError::Ai(format!(
            "AI returned unknown command: {}",
            route.command
        )));
    }

    Ok(AiResponse::SingleRoute(route))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::agent::Risk;

    #[test]
    fn parse_single_route() {
        let known = &["web", "yt", "open", "run"];
        let resp = parse_ai_response(
            r#"{"command": "web", "args": "rust tutorials"}"#,
            known,
            "rust tutorials",
        )
        .unwrap();
        match resp {
            AiResponse::SingleRoute(route) => {
                assert_eq!(route.command, "web");
                assert_eq!(route.args, "rust tutorials");
            }
            AiResponse::Plan(_) => panic!("Expected SingleRoute"),
        }
    }

    #[test]
    fn parse_with_code_fences() {
        let known = &["yt"];
        let resp = parse_ai_response(
            "```json\n{\"command\": \"yt\", \"args\": \"lofi\"}\n```",
            known,
            "play lofi",
        )
        .unwrap();
        match resp {
            AiResponse::SingleRoute(route) => {
                assert_eq!(route.command, "yt");
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
                {"command": "run", "args": "cargo init my-project", "label": "Create project", "risk": "moderate"},
                {"command": "run", "args": "code my-project", "label": "Open in VS Code", "risk": "safe"}
            ]}"#,
            known,
            "create rust project and open in vscode",
        )
        .unwrap();
        match resp {
            AiResponse::Plan(plan) => {
                assert_eq!(plan.steps.len(), 2);
                assert_eq!(plan.steps[0].command, "run");
                assert_eq!(plan.steps[0].risk, Risk::Moderate);
                assert_eq!(plan.steps[1].command, "run");
            }
            AiResponse::SingleRoute(_) => panic!("Expected Plan"),
        }
    }

    #[test]
    fn single_step_plan_collapses_to_route() {
        let known = &["run"];
        let resp = parse_ai_response(
            r#"{"steps": [{"command": "run", "args": "code .", "label": "Open VS Code", "risk": "safe"}]}"#,
            known,
            "open vscode",
        )
        .unwrap();
        match resp {
            AiResponse::SingleRoute(route) => {
                assert_eq!(route.command, "run");
                assert_eq!(route.args, "code .");
            }
            AiResponse::Plan(_) => panic!("Expected SingleRoute (collapsed)"),
        }
    }

    #[test]
    fn parse_unknown_command() {
        let known = &["web", "yt"];
        let result = parse_ai_response(
            r#"{"command": "delete", "args": "everything"}"#,
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
                {"command": "run", "args": "rm -rf /tmp/foo", "label": "Clean up", "risk": "safe"},
                {"command": "run", "args": "ls", "label": "List files", "risk": "safe"}
            ]}"#,
            known,
            "clean up and list files",
        )
        .unwrap();
        match resp {
            AiResponse::Plan(plan) => {
                assert_eq!(plan.steps[0].risk, Risk::Dangerous); // upgraded from safe
                assert_eq!(plan.steps[1].risk, Risk::Safe); // stays safe
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

    #[test]
    fn system_prompt_contains_steps_format() {
        let prompt = system_prompt(&["web", "run"]);
        assert!(prompt.contains("steps"));
        assert!(prompt.contains("risk"));
    }
}

use crate::action_registry::RiskLevel;
use crate::error::LychiError;
use crate::providers::{AgentPlan, AgentStep, AiResponse, AiRoute, generate_plan_id};
use crate::rules::shell::ShellRules;

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

Rules:
- "open" = GUI apps by name. "project" = open a project folder in editor. "browse" = browse a directory. "run" = shell commands.
- For file searches by name/type, use "run" with ls or find — not "browse".
- When uncertain between two handlers, prefer the more specific one.
- Extract clean arguments — strip filler words ("please", "can you", "I want to").
- Respond with ONLY valid JSON. No extra text.
- Fallback: question → "ask", non-question → "web".{context_section}"#
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
            "System power controls. Args: shutdown, reboot, suspend, hibernate, lock, logout"
        }
        "note" => "Quick sticky note. Args: text to save, or 'read' to view current note",
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
}

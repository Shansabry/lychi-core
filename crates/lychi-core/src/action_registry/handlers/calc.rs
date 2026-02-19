use async_trait::async_trait;

use crate::action_registry::{ActionHandler, ActionResult, CompletionItem};
use crate::error::LychiError;

pub struct CalcHandler;

impl Default for CalcHandler {
    fn default() -> Self {
        Self::new()
    }
}

impl CalcHandler {
    pub fn new() -> Self {
        Self
    }

    fn evaluate(expr: &str) -> Option<f64> {
        meval::eval_str(expr).ok()
    }

    fn format_result(value: f64) -> String {
        if value.fract() == 0.0 && value.abs() < 1e15 {
            format!("{}", value as i64)
        } else {
            // Trim trailing zeros but keep reasonable precision
            let s = format!("{:.10}", value);
            let s = s.trim_end_matches('0');
            let s = s.trim_end_matches('.');
            s.to_string()
        }
    }
}

#[async_trait]
impl ActionHandler for CalcHandler {
    fn id(&self) -> &str {
        "calc"
    }

    fn description(&self) -> &str {
        "Evaluate a math expression"
    }

    async fn execute(&self, args: &str) -> Result<ActionResult, LychiError> {
        let expr = args.trim();
        if expr.is_empty() {
            return Ok(ActionResult {
                success: false,
                output: None,
                error: Some("Usage: calc <expression> or =<expression>".to_string()),
                duration_ms: 0,
                routed_by: None,
                open_url: None,
                needs_confirmation: None,
                risk_level: None,
                output_type: None,
                executed_args: None,
            });
        }

        match Self::evaluate(expr) {
            Some(result) => Ok(ActionResult {
                success: true,
                output: Some(Self::format_result(result)),
                error: None,
                duration_ms: 0,
                routed_by: None,
                open_url: None,
                needs_confirmation: None,
                risk_level: None,
                output_type: None,
                executed_args: None,
            }),
            None => Ok(ActionResult {
                success: false,
                output: None,
                error: Some(format!("Invalid expression: {expr}")),
                duration_ms: 0,
                routed_by: None,
                open_url: None,
                needs_confirmation: None,
                risk_level: None,
                output_type: None,
                executed_args: None,
            }),
        }
    }

    async fn completions(&self, partial: &str) -> Vec<CompletionItem> {
        let expr = partial.trim();
        if expr.is_empty() {
            return Vec::new();
        }

        // Show live result as a completion
        if let Some(result) = Self::evaluate(expr) {
            vec![CompletionItem {
                label: format!("= {}", Self::format_result(result)),
                icon_path: None,
                score: 1000,
                description: None,
            }]
        } else {
            Vec::new()
        }
    }
}

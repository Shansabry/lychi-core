use crate::state::AppState;
use lychi_core::action_registry::ActionResult;
use lychi_core::error::LychiError;
use lychi_core::providers::AgentPlan;
use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

#[tauri::command]
pub async fn get_agent_plan(
    input: String,
    state: State<'_, AppState>,
) -> Result<Option<AgentPlan>, LychiError> {
    let executor = state.executor.read().await;
    Ok(executor.try_plan(&input).await)
}

#[derive(Clone, Serialize)]
pub struct StepEvent {
    pub plan_id: String,
    pub step_index: usize,
    pub status: String,
    pub result: Option<ActionResult>,
}

#[tauri::command]
pub async fn execute_agent_plan(
    plan_id: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), LychiError> {
    let plan = {
        let pending = state.pending_plan.read().await;
        pending
            .as_ref()
            .filter(|p| p.id == plan_id)
            .cloned()
            .ok_or_else(|| LychiError::ExecutionFailed("No pending plan found".into()))?
    };

    let executor = state.executor.read().await;

    for (i, step) in plan.steps.iter().enumerate() {
        // Emit running status
        let _ = app.emit(
            "lychi://agent-step",
            StepEvent {
                plan_id: plan_id.clone(),
                step_index: i,
                status: "running".to_string(),
                result: None,
            },
        );

        // Execute step through the executor pipeline (resolve → validate → execute)
        // Plan steps are pre-confirmed (confirmed=true) since user approved the plan
        let result = executor
            .run(&format!("{} {}", step.action_id, step.args), true)
            .await
            .unwrap_or_else(|e| ActionResult {
                success: false,
                output: None,
                error: Some(e.to_string()),
                duration_ms: 0,
                routed_by: None,
                open_url: None,
                needs_confirmation: None,
                risk_level: None,
                output_type: None,
                executed_args: None,
            });

        let failed = !result.success;

        let _ = app.emit(
            "lychi://agent-step",
            StepEvent {
                plan_id: plan_id.clone(),
                step_index: i,
                status: if failed {
                    "failed".to_string()
                } else {
                    "done".to_string()
                },
                result: Some(result),
            },
        );

        // Stop on failure
        if failed {
            break;
        }
    }

    // Clear the pending plan
    let mut pending = state.pending_plan.write().await;
    *pending = None;

    Ok(())
}

/// Store a plan as pending so execute_agent_plan can find it.
#[tauri::command]
pub async fn store_agent_plan(
    plan: AgentPlan,
    state: State<'_, AppState>,
) -> Result<(), LychiError> {
    let mut pending = state.pending_plan.write().await;
    *pending = Some(plan);
    Ok(())
}

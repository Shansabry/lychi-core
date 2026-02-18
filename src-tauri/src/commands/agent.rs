use crate::state::AppState;
use lychi_core::ai::agent::AgentPlan;
use lychi_core::command::CommandResult;
use lychi_core::error::LychiError;
use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

#[tauri::command]
pub async fn get_agent_plan(
    input: String,
    state: State<'_, AppState>,
) -> Result<Option<AgentPlan>, LychiError> {
    let registry = state.registry.read().await;
    Ok(registry.try_plan(&input).await)
}

#[derive(Clone, Serialize)]
pub struct StepEvent {
    pub plan_id: String,
    pub step_index: usize,
    pub status: String,
    pub result: Option<CommandResult>,
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

    let registry = state.registry.read().await;

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

        let result = registry
            .execute_handler(&step.command, &step.args)
            .await
            .unwrap_or_else(|e| CommandResult {
                success: false,
                output: None,
                error: Some(e.to_string()),
                duration_ms: 0,
                routed_by: None,
                open_url: None,
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

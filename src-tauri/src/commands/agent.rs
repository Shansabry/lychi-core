use crate::state::AppState;
use lychi_core::action_registry::CommandResultDto;
use lychi_core::error::LychiError;
use lychi_core::providers::AgentPlan;
use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

#[tauri::command]
#[specta::specta]
pub async fn get_agent_plan(
    input: String,
    state: State<'_, AppState>,
) -> Result<Option<AgentPlan>, LychiError> {
    let executor = state.executor.read().await;
    Ok(executor.try_plan(&input).await)
}

#[derive(Clone, Serialize, specta::Type)]
pub struct StepEvent {
    pub plan_id: String,
    pub step_index: usize,
    pub status: String,
    pub result: Option<CommandResultDto>,
}

#[tauri::command]
#[specta::specta]
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

    // Config first, released before the executor guard is taken.
    let privacy = state.config_snapshot(|c| c.privacy.clone()).await;
    // Snapshot-then-release: a multi-step plan is the LONGEST possible hold —
    // never keep the guard across it (see `Executor`'s Clone note).
    let executor = state.executor.read().await.clone();

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

        // Execute step through the executor pipeline (resolve → validate → execute).
        // Plan steps run confirmed=true because the user approved the plan AFTER
        // seeing each step's LITERAL action_id + args in AgentPlanPanel (risk
        // badges flag which steps modify the system). Two safety nets survive
        // confirmed=true: (1) the executor enforces `Deny` regardless of confirmed
        // (run_inner), and (2) the shell hard-deny fires at the point the shell
        // string is assembled (shell_exec::reject_if_denied), so a catastrophic
        // command (rm -rf /, disk wipe, curl|sh) an AI hallucinates into a step
        // is blocked even here. confirmed=true only skips the *Confirm* prompt for
        // steps the user already vetted on screen — it never bypasses a hard deny.
        let exec = executor
            .run(
                &format!("{} {}", step.action_id, step.args),
                true,
                &privacy,
                &lychi_core::executor::RunInputs::default(),
            )
            .await;

        let result: CommandResultDto = match exec {
            Ok(exec) => {
                // Notify frontend when notes/todos/reminders are mutated by a plan step
                if exec.result.success
                    && matches!(exec.action_id.as_str(), "note" | "todo" | "reminder")
                {
                    let _ = app.emit("lychi://notes-changed", ());
                }
                CommandResultDto::build(exec.result, exec.envelope)
            }
            Err(e) => CommandResultDto {
                success: false,
                error: Some(e.to_string()),
                ..Default::default()
            },
        };

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
#[specta::specta]
pub async fn store_agent_plan(
    plan: AgentPlan,
    state: State<'_, AppState>,
) -> Result<(), LychiError> {
    let mut pending = state.pending_plan.write().await;
    *pending = Some(plan);
    Ok(())
}

//! Execution concurrency policy (G4), extracted from the Executor so the
//! orchestrator stays focused on sequencing and policy and doesn't accumulate
//! specialized concurrency mechanics (the "god object" risk).
//!
//! The `ConcurrencyGate` enforces each handler's declared [`ExecutionMode`]:
//! - `Immediate` — run directly, unbounded parallelism.
//! - `Exclusive` — `try_lock`; if held, reject with "busy" (fail-fast, not a
//!   queue — a launcher prefers "try again" over a silent wait).
//! - `ReplacePrevious` — install a cancel handle, fire the previous one, and race
//!   the call against cancellation so a newer invocation aborts the older one's
//!   in-flight future (genuinely cancelling the reqwest call, not just discarding
//!   the result).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::Notify;

use crate::action_registry::{ActionHandler, ActionResult, ExecContext, ExecutionMode};
use crate::error::LychiError;

/// Owns the concurrency primitives and enforces `ExecutionMode`. Held by the
/// Executor; nothing else touches these primitives.
#[derive(Default)]
pub struct ConcurrencyGate {
    /// Held for the duration of an `Exclusive` action so nothing else runs
    /// alongside it. Async mutex because the guard is held across the handler's
    /// await.
    exclusive: Arc<tokio::sync::Mutex<()>>,
    /// Per-handler cancel handles for `ReplacePrevious`. A newer invocation
    /// installs a fresh `Notify` and fires the previous one. std Mutex — never
    /// held across an await (see `run`).
    replace_cancel: Mutex<HashMap<String, Arc<Notify>>>,
}

impl ConcurrencyGate {
    pub fn new() -> Self {
        Self::default()
    }

    /// Run `handler.execute(ctx, args)` under its declared `ExecutionMode`.
    ///
    /// Returns `(result, busy)`. `busy` is true ONLY when an `Exclusive` action
    /// was rejected without running (so the caller can, e.g., reinsert a pending
    /// confirmation instead of consuming it).
    pub async fn run(
        &self,
        handler: &dyn ActionHandler,
        ctx: &ExecContext,
        args: &str,
    ) -> Result<(ActionResult, bool), LychiError> {
        match handler.execution_mode() {
            ExecutionMode::Immediate => Ok((handler.execute(ctx, args).await?, false)),
            ExecutionMode::Exclusive => self.run_exclusive(handler, ctx, args).await,
            ExecutionMode::ReplacePrevious => self.run_replace_previous(handler, ctx, args).await,
        }
    }

    async fn run_exclusive(
        &self,
        handler: &dyn ActionHandler,
        ctx: &ExecContext,
        args: &str,
    ) -> Result<(ActionResult, bool), LychiError> {
        let Ok(_guard) = self.exclusive.try_lock() else {
            return Ok((
                ActionResult::err(
                    "Busy — another exclusive action is running. Try again in a moment.",
                ),
                true,
            ));
        };
        Ok((handler.execute(ctx, args).await?, false))
    }

    async fn run_replace_previous(
        &self,
        handler: &dyn ActionHandler,
        ctx: &ExecContext,
        args: &str,
    ) -> Result<(ActionResult, bool), LychiError> {
        let id = handler.id().to_string();

        // Install a fresh cancel handle for THIS invocation and fire the previous
        // one (if any). The std-Mutex guard lives only inside this block — NEVER
        // across an await (a !Send guard across await would make the future !Send
        // and could deadlock).
        let my_cancel = Arc::new(Notify::new());
        {
            let mut map = self.replace_cancel.lock().unwrap();
            if let Some(prev) = map.insert(id.clone(), my_cancel.clone()) {
                // `notify_one` stores a permit if the superseded task hasn't
                // started awaiting yet — no lost wakeup.
                prev.notify_one();
            }
        }

        // Register the waiter (pin) BEFORE the select so it isn't recreated.
        let notified = my_cancel.notified();
        tokio::pin!(notified);

        tokio::select! {
            res = handler.execute(ctx, args) => {
                // Completed first. Remove our handle only if the map still points
                // at OUR Arc — a newer invocation may have already replaced it, and
                // we must not clobber its handle.
                let mut map = self.replace_cancel.lock().unwrap();
                if map.get(&id).map(Arc::as_ptr) == Some(Arc::as_ptr(&my_cancel)) {
                    map.remove(&id);
                }
                Ok((res?, false))
            }
            _ = &mut notified => {
                // Superseded: dropping the `handler.execute(..)` future here drops
                // the in-flight reqwest send()/json() future → connection closed,
                // request aborted. No further waste.
                tracing::debug!("[execute] {id} superseded — aborting in-flight call");
                Ok((
                    ActionResult {
                        success: false,
                        ..Default::default()
                    },
                    false,
                ))
            }
        }
    }
}

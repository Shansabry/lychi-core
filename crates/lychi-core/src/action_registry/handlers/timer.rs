use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use async_trait::async_trait;
use redb::{ReadableDatabase, ReadableTable};
use serde::Serialize;

use crate::action_registry::{
    ActionHandler, ActionResult, CommandCategory, CompletionItem, ExecContext, OutputType,
};
use crate::error::LychiError;

/// In-memory timer state, shared between handler + background tick loop.
pub type TimerState = Arc<Mutex<HashMap<String, Timer>>>;

/// Create a new empty timer state.
pub fn new_timer_state() -> TimerState {
    Arc::new(Mutex::new(HashMap::new()))
}

/// Maximum concurrent timers.
const MAX_TIMERS: usize = 10;

#[derive(Debug, Clone)]
pub struct Timer {
    /// Display name (user-supplied or "timer").
    pub name: String,
    /// Total duration in seconds.
    pub duration_secs: u64,
    /// When the timer was started (or last resumed).
    pub started_at: Instant,
    /// Accumulated elapsed seconds before the current run (from pauses).
    pub elapsed_before_secs: f64,
    /// If paused, when it was paused.
    pub paused_at: Option<Instant>,
    /// Whether the timer has fired its completion notification.
    pub completed: bool,
}

impl Timer {
    /// Elapsed seconds (including paused time).
    pub fn elapsed_secs(&self) -> f64 {
        if let Some(paused) = self.paused_at {
            self.elapsed_before_secs + paused.duration_since(self.started_at).as_secs_f64()
        } else {
            self.elapsed_before_secs + self.started_at.elapsed().as_secs_f64()
        }
    }

    /// Remaining seconds (clamped to 0).
    pub fn remaining_secs(&self) -> f64 {
        (self.duration_secs as f64 - self.elapsed_secs()).max(0.0)
    }

    pub fn is_paused(&self) -> bool {
        self.paused_at.is_some()
    }

    pub fn is_stopwatch(&self) -> bool {
        self.duration_secs == 0
    }

    pub fn is_done(&self) -> bool {
        !self.is_stopwatch() && self.elapsed_secs() >= self.duration_secs as f64
    }

    /// Convert to the persistable wall-clock representation.
    fn to_entry(&self) -> crate::db::schema::TimerEntry {
        // If running, the current run's wall-clock start = now - (Instant elapsed
        // of this run). If paused, there is no active run.
        let running_since_epoch_ms = if self.paused_at.is_some() {
            None
        } else {
            let this_run_secs = self.started_at.elapsed().as_secs_f64();
            let started_ms =
                crate::db::now_millis().saturating_sub((this_run_secs * 1000.0) as u64);
            Some(started_ms)
        };
        // When paused, fold the paused run's elapsed into elapsed_before so the
        // entry fully captures accumulated time with no active run.
        let elapsed_before_secs = if let Some(paused) = self.paused_at {
            self.elapsed_before_secs + paused.duration_since(self.started_at).as_secs_f64()
        } else {
            self.elapsed_before_secs
        };
        crate::db::schema::TimerEntry {
            name: self.name.clone(),
            duration_secs: self.duration_secs,
            elapsed_before_secs,
            running_since_epoch_ms,
        }
    }

    /// Reconstruct a live `Timer` from its persisted form, mapping the stored
    /// wall-clock start back onto the monotonic `Instant` clock.
    fn from_entry(e: &crate::db::schema::TimerEntry) -> Self {
        let now = Instant::now();
        match e.running_since_epoch_ms {
            // Running: place started_at in the past by the wall-clock elapsed
            // since the stored start, so remaining time continues correctly.
            Some(started_ms) => {
                let elapsed_ms = crate::db::now_millis().saturating_sub(started_ms);
                let started_at = now
                    .checked_sub(std::time::Duration::from_millis(elapsed_ms))
                    .unwrap_or(now);
                Timer {
                    name: e.name.clone(),
                    duration_secs: e.duration_secs,
                    started_at,
                    elapsed_before_secs: e.elapsed_before_secs,
                    paused_at: None,
                    completed: false,
                }
            }
            // Paused: no active run; started_at == paused_at == now so this run
            // contributes zero, and all elapsed lives in elapsed_before_secs.
            None => Timer {
                name: e.name.clone(),
                duration_secs: e.duration_secs,
                started_at: now,
                elapsed_before_secs: e.elapsed_before_secs,
                paused_at: Some(now),
                completed: false,
            },
        }
    }
}

/// Persist the entire timer map to redb (called after every mutation). Timers
/// are few and small, so a full rewrite each time is simplest and cheap.
pub fn persist_timers(state: &TimerState, db: &Arc<redb::Database>) {
    let snapshot: Vec<(String, crate::db::schema::TimerEntry)> = {
        let Ok(timers) = state.lock() else {
            return;
        };
        timers
            .iter()
            // Don't persist already-completed timers (they're about to be removed).
            .filter(|(_, t)| !t.completed)
            .map(|(id, t)| (id.clone(), t.to_entry()))
            .collect()
    };

    let write = || -> Result<(), LychiError> {
        let txn = db.begin_write()?;
        {
            let mut table = txn.open_table(crate::db::TIMERS)?;
            // Clear then rewrite: collect existing keys, remove, insert current.
            let existing: Vec<String> = table
                .iter()?
                .filter_map(|r| r.ok().map(|(k, _)| k.value().to_string()))
                .collect();
            for k in existing {
                table.remove(k.as_str())?;
            }
            for (id, entry) in &snapshot {
                let bytes = postcard::to_allocvec(entry)
                    .map_err(|e| LychiError::Database(e.to_string()))?;
                table.insert(id.as_str(), bytes.as_slice())?;
            }
        }
        txn.commit()?;
        Ok(())
    };
    if let Err(e) = write() {
        tracing::warn!("[timer] failed to persist timers: {e}");
    }
}

/// Load persisted timers at startup, reconstructing live `Timer`s. Timers that
/// already completed while the app was closed are dropped (a fire-on-boot could
/// be added later, but silently dropping a long-expired timer is the safer
/// default than a surprise notification for something the user forgot).
pub fn load_timers(db: &Arc<redb::Database>) -> HashMap<String, Timer> {
    let mut map = HashMap::new();
    let mut read = || -> Result<(), LychiError> {
        let txn = db.begin_read()?;
        let table = txn.open_table(crate::db::TIMERS)?;
        for result in table.iter()? {
            let (key, val) = result?;
            // One unreadable row must not drop every running timer.
            let Some(entry) = crate::db::decode_row::<crate::db::schema::TimerEntry>(
                "timers",
                key.value(),
                val.value(),
            ) else {
                continue;
            };
            let timer = Timer::from_entry(&entry);
            // Skip countdowns that already elapsed while the app was closed.
            if timer.is_done() {
                continue;
            }
            map.insert(key.value().to_string(), timer);
        }
        Ok(())
    };
    if let Err(e) = read() {
        tracing::warn!("[timer] failed to load timers: {e}");
    }
    map
}

/// Serializable timer status sent to the frontend.
#[derive(Debug, Clone, Serialize, specta::Type)]
pub struct TimerStatus {
    pub id: String,
    pub name: String,
    pub duration_secs: u64,
    pub remaining_secs: f64,
    pub elapsed_secs: f64,
    pub paused: bool,
    pub done: bool,
    pub stopwatch: bool,
}

/// Get a snapshot of all active timers.
pub fn get_all_timers(state: &TimerState) -> Vec<TimerStatus> {
    let timers = state.lock().unwrap();
    let mut list: Vec<TimerStatus> = timers
        .iter()
        .map(|(id, t)| TimerStatus {
            id: id.clone(),
            name: t.name.clone(),
            duration_secs: t.duration_secs,
            remaining_secs: t.remaining_secs(),
            elapsed_secs: t.elapsed_secs(),
            paused: t.is_paused(),
            done: t.is_done(),
            stopwatch: t.is_stopwatch(),
        })
        .collect();
    // Stopwatches sort to the end, timers sorted by remaining time
    list.sort_by(|a, b| {
        a.stopwatch
            .cmp(&b.stopwatch)
            .then_with(|| a.remaining_secs.partial_cmp(&b.remaining_secs).unwrap())
    });
    list
}

/// Parse a duration string like "25m", "5m30s", "90s", "1h", "30" (seconds), "1:30",
/// "25mins", "2hrs", "30secs".
fn parse_duration(s: &str) -> Option<u64> {
    let s = s.trim().to_lowercase();

    // Try "H:MM" or "M:SS" format (timer-specific, not used in reminders)
    if let Some((a, b)) = s.split_once(':') {
        let a: u64 = a.trim().parse().ok()?;
        let b: u64 = b.trim().parse().ok()?;
        // If first part > 59, treat as H:MM, otherwise M:SS
        if a > 59 {
            return Some(a * 3600 + b * 60);
        }
        return Some(a * 60 + b);
    }

    // Shared duration parser: handles "25m", "1h30m", "25mins", "2hours",
    // "30 minutes", "1 hour and 30 minutes", bare numbers as minutes, etc.
    crate::reminders::time_parse::parse_duration_secs(&s)
}

/// Format seconds as human-readable "Xm Ys" or "Xh Ym".
fn format_duration(secs: f64) -> String {
    let total = secs.ceil() as u64;
    if total >= 3600 {
        let h = total / 3600;
        let m = (total % 3600) / 60;
        let s = total % 60;
        if s > 0 {
            format!("{h}h {m}m {s}s")
        } else if m > 0 {
            format!("{h}h {m}m")
        } else {
            format!("{h}h")
        }
    } else if total >= 60 {
        let m = total / 60;
        let s = total % 60;
        if s > 0 {
            format!("{m}m {s}s")
        } else {
            format!("{m}m")
        }
    } else {
        format!("{total}s")
    }
}

fn ok_result(start: Instant, output: String) -> ActionResult {
    ActionResult::ok(output, OutputType::Status).with_duration(start.elapsed().as_millis() as u64)
}

fn err_result(start: Instant, error: String) -> ActionResult {
    ActionResult::err(error).with_duration(start.elapsed().as_millis() as u64)
}

pub struct TimerHandler {
    state: TimerState,
    db: Arc<redb::Database>,
}

impl TimerHandler {
    pub fn new(state: TimerState, db: Arc<redb::Database>) -> Self {
        Self { state, db }
    }

    /// Persist the current timer map after a mutation. Best-effort.
    fn persist(&self) {
        persist_timers(&self.state, &self.db);
    }
}

const TIMER_SUBCOMMANDS: &[(&str, &str)] = &[
    ("start", "Start a new timer (e.g. timer start 25m)"),
    ("stopwatch", "Start a stopwatch (counts up)"),
    ("stop", "Stop and remove a timer"),
    ("pause", "Pause a running timer"),
    ("resume", "Resume a paused timer"),
    ("status", "Show all active timers"),
    ("clear", "Remove all timers"),
];

#[async_trait]
impl ActionHandler for TimerHandler {
    fn triggers(&self) -> &'static [crate::action_registry::Trigger] {
        use crate::action_registry::{ArgTransform, Trigger};
        static TRIGGERS: &[Trigger] = &[
            Trigger::keywords(&["timer"]),
            Trigger::new(&["stopwatch"], ArgTransform::Prepend("stopwatch")),
        ];
        TRIGGERS
    }

    fn id(&self) -> &str {
        "timer"
    }

    fn description(&self) -> &str {
        "Timer — countdown timers with desktop notification. Usage: timer 25m, timer start workout 5m, timer stop, timer status"
    }
    fn category(&self) -> CommandCategory {
        CommandCategory::Utilities
    }

    async fn execute(&self, ctx: &ExecContext, args: &str) -> Result<ActionResult, LychiError> {
        let start = Instant::now();
        let trimmed = args.trim();

        // No args → open timer view
        if trimmed.is_empty() {
            return Ok(ok_result(start, "__timer_panel__".into()));
        }

        let (cmd, rest) = trimmed.split_once(' ').unwrap_or((trimmed, ""));
        let rest = rest.trim();

        // Persist after any command that can mutate the timer map, so running
        // timers survive a restart. `status` is the only read-only verb.
        let verb = cmd.to_lowercase();
        let mutating = verb != "status";
        // `cmd` is the original-cased first token — needed so the bare
        // `timer <name> <dur>` form keeps the name's casing.
        let result = self.execute_verb(ctx, &verb, cmd, rest, start).await;
        if mutating {
            self.persist();
        }
        result
    }

    async fn completions(&self, partial: &str) -> Vec<CompletionItem> {
        self.completions_impl(partial).await
    }
}

impl TimerHandler {
    async fn execute_verb(
        &self,
        ctx: &ExecContext,
        verb: &str,
        cmd: &str,
        rest: &str,
        start: Instant,
    ) -> Result<ActionResult, LychiError> {
        match verb {
            "start" => {
                // Parse: "timer start 25m" or "timer start workout 5m"
                // Last token is the duration, everything before is the name
                let tokens: Vec<&str> = rest.split_whitespace().collect();
                if tokens.is_empty() {
                    return Ok(err_result(
                        start,
                        "Usage: timer start [name] <duration>\nExamples: timer start 25m, timer start workout 5m, timer start 1:30".into(),
                    ));
                }

                let (name, duration_str) = if tokens.len() == 1 {
                    ("Timer".to_string(), tokens[0])
                } else {
                    // Last token is duration, rest is name
                    let dur = tokens[tokens.len() - 1];
                    let name = tokens[..tokens.len() - 1].join(" ");
                    (name, dur)
                };

                let duration_secs = match parse_duration(duration_str) {
                    Some(d) => d,
                    None => {
                        return Ok(err_result(
                            start,
                            format!(
                                "Can't parse duration: \"{duration_str}\"\nExamples: 25m, 5m30s, 90s, 1h, 1:30"
                            ),
                        ));
                    }
                };

                let id = crate::db::new_id();
                let timer = Timer {
                    name: name.clone(),
                    duration_secs,
                    started_at: Instant::now(),
                    elapsed_before_secs: 0.0,
                    paused_at: None,
                    completed: false,
                };

                let mut timers = self.state.lock().unwrap();
                if timers.len() >= MAX_TIMERS {
                    return Ok(err_result(
                        start,
                        format!("Max {MAX_TIMERS} concurrent timers"),
                    ));
                }
                timers.insert(id, timer);

                Ok(ok_result(start, "__timer_panel__".into()))
            }

            "stop" | "cancel" | "remove" | "rm" | "delete" => {
                if rest.is_empty() {
                    let mut timers = self.state.lock().unwrap();
                    if timers.len() == 1 {
                        timers.clear();
                        return Ok(ok_result(start, "Timer stopped".into()));
                    } else if timers.is_empty() {
                        return Ok(ok_result(start, "No active timers".into()));
                    } else {
                        return Ok(err_result(
                            start,
                            "Multiple timers — specify which: timer stop <name>".into(),
                        ));
                    }
                }

                let lower = rest.to_lowercase();
                let mut timers = self.state.lock().unwrap();
                let key = timers
                    .iter()
                    .find(|(_, t)| t.name.to_lowercase() == lower)
                    .map(|(k, _)| k.clone());
                if let Some(k) = key {
                    timers.remove(&k);
                    // If timers remain, show the panel; otherwise show status message
                    if timers.is_empty() {
                        Ok(ok_result(start, "Timer stopped".into()))
                    } else {
                        Ok(ok_result(start, "__timer_panel__".into()))
                    }
                } else {
                    Ok(err_result(start, format!("No timer named \"{rest}\"")))
                }
            }

            "pause" => {
                let mut timers = self.state.lock().unwrap();
                let target = if rest.is_empty() {
                    // Pause the first running timer
                    timers
                        .iter_mut()
                        .find(|(_, t)| !t.is_paused() && !t.is_done())
                        .map(|(_, t)| t)
                } else {
                    let lower = rest.to_lowercase();
                    timers.values_mut().find(|t| t.name.to_lowercase() == lower)
                };

                match target {
                    Some(t) if t.is_paused() => Ok(ok_result(start, "__timer_panel__".into())),
                    Some(t) => {
                        t.paused_at = Some(Instant::now());
                        Ok(ok_result(start, "__timer_panel__".into()))
                    }
                    None => Ok(err_result(start, "No running timer to pause".into())),
                }
            }

            "resume" | "unpause" | "continue" => {
                let mut timers = self.state.lock().unwrap();
                let target = if rest.is_empty() {
                    timers
                        .iter_mut()
                        .find(|(_, t)| t.is_paused())
                        .map(|(_, t)| t)
                } else {
                    let lower = rest.to_lowercase();
                    timers.values_mut().find(|t| t.name.to_lowercase() == lower)
                };

                match target {
                    Some(t) if !t.is_paused() => Ok(ok_result(start, "__timer_panel__".into())),
                    Some(t) => {
                        if let Some(paused) = t.paused_at.take() {
                            t.elapsed_before_secs +=
                                paused.duration_since(t.started_at).as_secs_f64();
                            t.started_at = Instant::now();
                        }
                        Ok(ok_result(start, "__timer_panel__".into()))
                    }
                    None => Ok(err_result(start, "No paused timer to resume".into())),
                }
            }

            "stopwatch" | "sw" => {
                // "timer stopwatch" or "timer sw" — start a stopwatch (count-up, no deadline)
                // "timer stopwatch workout" — start named stopwatch
                let name = if rest.is_empty() {
                    "Stopwatch".to_string()
                } else {
                    // Check for "stop <name>" or "start <name>"
                    let (sub, sub_rest) = rest.split_once(' ').unwrap_or((rest, ""));
                    match sub.to_lowercase().as_str() {
                        "start" => {
                            if sub_rest.is_empty() {
                                "Stopwatch".to_string()
                            } else {
                                sub_rest.to_string()
                            }
                        }
                        "stop" => {
                            // Delegate to stop handler
                            let target = if sub_rest.is_empty() {
                                // Stop any stopwatch
                                let mut timers = self.state.lock().unwrap();
                                let key = timers
                                    .iter()
                                    .find(|(_, t)| t.is_stopwatch())
                                    .map(|(k, _)| k.clone());
                                if let Some(k) = key {
                                    timers.remove(&k);
                                    return if timers.is_empty() {
                                        Ok(ok_result(start, "Stopwatch stopped".into()))
                                    } else {
                                        Ok(ok_result(start, "__timer_panel__".into()))
                                    };
                                }
                                return Ok(err_result(start, "No active stopwatch".into()));
                            } else {
                                sub_rest.to_lowercase()
                            };
                            let mut timers = self.state.lock().unwrap();
                            let key = timers
                                .iter()
                                .find(|(_, t)| t.is_stopwatch() && t.name.to_lowercase() == target)
                                .map(|(k, _)| k.clone());
                            if let Some(k) = key {
                                timers.remove(&k);
                                return if timers.is_empty() {
                                    Ok(ok_result(start, "Stopwatch stopped".into()))
                                } else {
                                    Ok(ok_result(start, "__timer_panel__".into()))
                                };
                            }
                            return Ok(err_result(
                                start,
                                format!("No stopwatch named \"{sub_rest}\""),
                            ));
                        }
                        "pause" | "resume" => {
                            // Delegate to existing pause/resume with stopwatch name filter
                            return self.execute(ctx, &format!("{sub} {sub_rest}")).await;
                        }
                        _ => rest.to_string(),
                    }
                };

                let id = crate::db::new_id();
                let timer = Timer {
                    name: name.clone(),
                    duration_secs: 0, // stopwatch = no deadline
                    started_at: Instant::now(),
                    elapsed_before_secs: 0.0,
                    paused_at: None,
                    completed: false,
                };

                let mut timers = self.state.lock().unwrap();
                if timers.len() >= MAX_TIMERS {
                    return Ok(err_result(
                        start,
                        format!("Max {MAX_TIMERS} concurrent timers"),
                    ));
                }
                timers.insert(id, timer);

                Ok(ok_result(start, "__timer_panel__".into()))
            }

            "status" | "list" | "ls" => Ok(ok_result(start, "__timer_panel__".into())),

            "clear" => {
                let mut timers = self.state.lock().unwrap();
                let count = timers.len();
                timers.clear();
                Ok(ok_result(
                    start,
                    if count == 0 {
                        "No timers to clear".into()
                    } else {
                        format!("Cleared {count} timer(s)")
                    },
                ))
            }

            // If first arg looks like a duration, treat as "timer start <duration>"
            _ => {
                // Try to parse cmd as duration (e.g. "timer 25m")
                if let Some(duration_secs) = parse_duration(cmd) {
                    // Rest is the name (optional)
                    let name = if rest.is_empty() {
                        "Timer".to_string()
                    } else {
                        rest.to_string()
                    };

                    let id = crate::db::new_id();
                    let timer = Timer {
                        name: name.clone(),
                        duration_secs,
                        started_at: Instant::now(),
                        elapsed_before_secs: 0.0,
                        paused_at: None,
                        completed: false,
                    };

                    let mut timers = self.state.lock().unwrap();
                    if timers.len() >= MAX_TIMERS {
                        return Ok(err_result(
                            start,
                            format!("Max {MAX_TIMERS} concurrent timers"),
                        ));
                    }
                    timers.insert(id, timer);

                    Ok(ok_result(start, "__timer_panel__".into()))
                } else if let Some(duration_secs) = parse_duration(rest) {
                    // "timer workout 5m" → name="workout", duration from rest
                    // But rest might be "5m" and cmd is the name
                    let name = cmd.to_string();
                    let id = crate::db::new_id();
                    let timer = Timer {
                        name: name.clone(),
                        duration_secs,
                        started_at: Instant::now(),
                        elapsed_before_secs: 0.0,
                        paused_at: None,
                        completed: false,
                    };

                    let mut timers = self.state.lock().unwrap();
                    if timers.len() >= MAX_TIMERS {
                        return Ok(err_result(
                            start,
                            format!("Max {MAX_TIMERS} concurrent timers"),
                        ));
                    }
                    timers.insert(id, timer);

                    Ok(ok_result(start, "__timer_panel__".into()))
                } else {
                    Ok(err_result(
                        start,
                        format!(
                            "Unknown timer command: \"{verb}\"\nUsage: timer 25m, timer start workout 5m, timer stop, timer status"
                        ),
                    ))
                }
            }
        }
    }

    async fn completions_impl(&self, partial: &str) -> Vec<CompletionItem> {
        let lower = partial.to_lowercase();

        // Show active timers as completions when relevant
        let timers = get_all_timers(&self.state);
        let mut items: Vec<CompletionItem> = Vec::new();

        // Show active timer status as top completions
        for t in &timers {
            let status = if t.done {
                "DONE"
            } else if t.paused {
                "paused"
            } else {
                "running"
            };
            let desc = if t.stopwatch {
                format!("{} elapsed ({status})", format_duration(t.elapsed_secs),)
            } else {
                format!("{} left ({status})", format_duration(t.remaining_secs),)
            };
            if lower.is_empty()
                || t.name.to_lowercase().contains(&lower)
                || "status".contains(&lower)
            {
                items.push(CompletionItem {
                    label: t.name.clone(),
                    icon_path: None,
                    score: 200,
                    description: Some(desc),
                    reason: None,
                    thumb_b64: None,
                    // A timer name isn't a runnable command on its own —
                    // selecting it shows timer status.
                    run: Some("timer status".to_string()),
                    ..Default::default()
                });
            }
        }

        // Show subcommands
        for &(cmd, desc) in TIMER_SUBCOMMANDS {
            if cmd.contains(&lower) || lower.is_empty() {
                items.push(CompletionItem {
                    label: cmd.to_string(),
                    icon_path: None,
                    score: if cmd.starts_with(&lower) { 100 } else { 50 },
                    description: Some(desc.to_string()),
                    reason: None,
                    thumb_b64: None,
                    run: Some(format!("timer {cmd}")),
                    ..Default::default()
                });
            }
        }

        items
    }
}

/// Background loop: checks timers every 500ms for completion, sends desktop notification.
/// Also checks reminders every ~10s (every 20th tick).
///
/// All notify-rust calls are serialized in this single thread to avoid
/// concurrent D-Bus access that causes heap corruption on Linux.
/// Runs on a dedicated OS thread until `running` is set to false.
/// Automatically recovers from panics (logs and restarts the poll loop).
pub fn run_timer_monitor(
    state: TimerState,
    db: std::sync::Arc<redb::Database>,
    running: std::sync::Arc<std::sync::atomic::AtomicBool>,
) {
    use std::sync::atomic::Ordering;
    tracing::info!("Timer monitor started");

    loop {
        if !running.load(Ordering::Relaxed) {
            break;
        }

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            timer_monitor_loop(&state, &db, &running);
        }));

        if let Err(_panic) = result {
            tracing::error!(
                "Timer monitor panicked — restarting in 1s \
                 (in-flight timers may have been lost)"
            );
            std::thread::sleep(std::time::Duration::from_secs(1));
        } else {
            // Loop returned normally — `running` is false, exit cleanly
            break;
        }
    }
    tracing::info!("Timer monitor stopped");
}

fn timer_monitor_loop(
    state: &TimerState,
    db: &std::sync::Arc<redb::Database>,
    running: &std::sync::Arc<std::sync::atomic::AtomicBool>,
) {
    use std::sync::atomic::Ordering;
    let reminder_store = crate::reminders::store::RemindersStore::new();
    let mut tick: u32 = 0;

    while running.load(Ordering::Relaxed) {
        std::thread::sleep(std::time::Duration::from_millis(500));
        tick = tick.wrapping_add(1);

        // --- Timer checks (every tick) ---
        {
            let mut timers = state.lock().unwrap();
            let mut to_remove = Vec::new();

            for (id, timer) in timers.iter_mut() {
                if timer.is_done() && !timer.completed {
                    timer.completed = true;
                    let name = timer.name.clone();
                    let duration = format_duration(timer.duration_secs as f64);

                    if let Err(e) = notify_rust::Notification::new()
                        .summary(&format!("Timer complete: {name}"))
                        .body(&format!("{name} — {duration} timer finished"))
                        .icon("alarm-timer")
                        .timeout(notify_rust::Timeout::Milliseconds(10000))
                        .show()
                    {
                        tracing::warn!("[timer] notification error: {e}");
                    }
                    tracing::info!("[timer] {name} ({duration}) completed");

                    to_remove.push(id.clone());
                }
            }

            let removed_any = !to_remove.is_empty();
            for id in to_remove {
                timers.remove(&id);
            }
            drop(timers);
            // A fired timer was removed → update the persisted set so it doesn't
            // resurrect on the next restart.
            if removed_any {
                persist_timers(state, db);
            }
        }

        // --- Reminder checks (every ~10s = 20 ticks) ---
        if tick.is_multiple_of(20) {
            crate::reminders::monitor::check_and_fire(&reminder_store, db);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action_registry::Output;

    /// Extract the text body from a result's output, for assertions.
    fn body(r: &ActionResult) -> Option<&str> {
        match &r.output {
            Output::Text { body, .. } => Some(body.as_str()),
            _ => None,
        }
    }

    #[test]
    fn test_parse_duration_minutes() {
        assert_eq!(parse_duration("25m"), Some(25 * 60));
        assert_eq!(parse_duration("5m"), Some(5 * 60));
        assert_eq!(parse_duration("1m"), Some(60));
    }

    #[test]
    fn persist_roundtrip_preserves_running_timer() {
        // A running 10-minute timer that's ~elapsed 100s: after to_entry →
        // from_entry the remaining time must be preserved (within a small delta).
        let t = Timer {
            name: "focus".into(),
            duration_secs: 600,
            started_at: Instant::now() - std::time::Duration::from_secs(100),
            elapsed_before_secs: 0.0,
            paused_at: None,
            completed: false,
        };
        let before = t.remaining_secs();
        let restored = Timer::from_entry(&t.to_entry());
        assert_eq!(restored.name, "focus");
        assert!(!restored.is_paused());
        // Remaining should match within 2s (wall-clock ms rounding + test time).
        assert!(
            (restored.remaining_secs() - before).abs() < 2.0,
            "remaining drifted: {before} → {}",
            restored.remaining_secs()
        );
    }

    #[test]
    fn persist_roundtrip_preserves_paused_timer() {
        // A paused timer: elapsed is frozen, and it must come back paused with
        // the same elapsed time regardless of wall-clock passing.
        let now = Instant::now();
        let t = Timer {
            name: "brew".into(),
            duration_secs: 300,
            started_at: now - std::time::Duration::from_secs(50),
            elapsed_before_secs: 0.0,
            paused_at: Some(now), // paused after 50s of running
            completed: false,
        };
        let elapsed_before = t.elapsed_secs();
        let restored = Timer::from_entry(&t.to_entry());
        assert!(restored.is_paused(), "should restore paused");
        assert!(
            (restored.elapsed_secs() - elapsed_before).abs() < 2.0,
            "paused elapsed drifted: {elapsed_before} → {}",
            restored.elapsed_secs()
        );
    }

    #[test]
    fn load_persist_via_db_roundtrips() {
        let db = crate::db::open_test_database();
        let state = new_timer_state();
        state.lock().unwrap().insert(
            "id1".to_string(),
            Timer {
                name: "washer".into(),
                duration_secs: 1800,
                started_at: Instant::now(),
                elapsed_before_secs: 0.0,
                paused_at: None,
                completed: false,
            },
        );
        persist_timers(&state, &db);

        let loaded = load_timers(&db);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded.get("id1").unwrap().name, "washer");
        assert_eq!(loaded.get("id1").unwrap().duration_secs, 1800);
    }

    #[test]
    fn test_parse_duration_seconds() {
        assert_eq!(parse_duration("90s"), Some(90));
        assert_eq!(parse_duration("30s"), Some(30));
    }

    #[test]
    fn test_parse_duration_combined() {
        assert_eq!(parse_duration("5m30s"), Some(5 * 60 + 30));
        assert_eq!(parse_duration("1h30m"), Some(90 * 60));
        assert_eq!(parse_duration("1h5m30s"), Some(3600 + 300 + 30));
    }

    #[test]
    fn test_parse_duration_hours() {
        assert_eq!(parse_duration("1h"), Some(3600));
        assert_eq!(parse_duration("2h"), Some(7200));
    }

    #[test]
    fn test_parse_duration_colon() {
        assert_eq!(parse_duration("1:30"), Some(90));
        assert_eq!(parse_duration("5:00"), Some(300));
        assert_eq!(parse_duration("25:00"), Some(1500));
    }

    #[test]
    fn test_parse_duration_bare_number() {
        // Bare numbers treated as minutes
        assert_eq!(parse_duration("25"), Some(25 * 60));
        assert_eq!(parse_duration("5"), Some(5 * 60));
    }

    #[test]
    fn test_parse_duration_invalid() {
        assert_eq!(parse_duration(""), None);
        assert_eq!(parse_duration("abc"), None);
        assert_eq!(parse_duration("0"), None);
        assert_eq!(parse_duration("0m"), None);
    }

    #[test]
    fn test_format_duration() {
        assert_eq!(format_duration(30.0), "30s");
        assert_eq!(format_duration(90.0), "1m 30s");
        assert_eq!(format_duration(300.0), "5m");
        assert_eq!(format_duration(3600.0), "1h");
        assert_eq!(format_duration(3661.0), "1h 1m 1s");
    }

    #[tokio::test]
    async fn test_timer_start_and_status() {
        let state = new_timer_state();
        let handler = TimerHandler::new(state.clone(), crate::db::open_test_database());

        let result = handler
            .execute(
                &crate::action_registry::ExecContext::default(),
                "start test 1m",
            )
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(body(&result), Some("__timer_panel__"));

        // Verify timer was actually created
        let timers = get_all_timers(&state);
        assert_eq!(timers.len(), 1);
        assert_eq!(timers[0].name, "test");
        assert_eq!(timers[0].duration_secs, 60);

        let result = handler
            .execute(&crate::action_registry::ExecContext::default(), "status")
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(body(&result), Some("__timer_panel__"));
    }

    #[tokio::test]
    async fn test_timer_pause_resume() {
        let state = new_timer_state();
        let handler = TimerHandler::new(state.clone(), crate::db::open_test_database());

        handler
            .execute(
                &crate::action_registry::ExecContext::default(),
                "start workout 5m",
            )
            .await
            .unwrap();

        let result = handler
            .execute(
                &crate::action_registry::ExecContext::default(),
                "pause workout",
            )
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(body(&result), Some("__timer_panel__"));
        assert!(get_all_timers(&state)[0].paused);

        let result = handler
            .execute(
                &crate::action_registry::ExecContext::default(),
                "resume workout",
            )
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(body(&result), Some("__timer_panel__"));
        assert!(!get_all_timers(&state)[0].paused);
    }

    #[tokio::test]
    async fn test_timer_stop() {
        let state = new_timer_state();
        let handler = TimerHandler::new(state.clone(), crate::db::open_test_database());

        handler
            .execute(
                &crate::action_registry::ExecContext::default(),
                "start cooking 30s",
            )
            .await
            .unwrap();

        let result = handler
            .execute(
                &crate::action_registry::ExecContext::default(),
                "stop cooking",
            )
            .await
            .unwrap();
        assert!(result.success);
        // Last timer removed — returns status message, not panel sentinel
        assert!(body(&result).unwrap().contains("stopped"));

        let timers = get_all_timers(&state);
        assert!(timers.is_empty());
    }

    #[tokio::test]
    async fn test_timer_shorthand() {
        let state = new_timer_state();
        let handler = TimerHandler::new(state.clone(), crate::db::open_test_database());

        // "timer 25m" → starts a timer and opens panel
        let result = handler
            .execute(&crate::action_registry::ExecContext::default(), "25m")
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(body(&result), Some("__timer_panel__"));
        assert_eq!(get_all_timers(&state).len(), 1);
    }

    #[tokio::test]
    async fn test_timer_clear() {
        let state = new_timer_state();
        let handler = TimerHandler::new(state.clone(), crate::db::open_test_database());

        handler
            .execute(
                &crate::action_registry::ExecContext::default(),
                "start a 5m",
            )
            .await
            .unwrap();
        handler
            .execute(
                &crate::action_registry::ExecContext::default(),
                "start b 10m",
            )
            .await
            .unwrap();

        let result = handler
            .execute(&crate::action_registry::ExecContext::default(), "clear")
            .await
            .unwrap();
        assert!(result.success);
        assert!(body(&result).unwrap().contains("Cleared 2"));

        let timers = get_all_timers(&state);
        assert!(timers.is_empty());
    }

    #[tokio::test]
    async fn test_stopwatch_start() {
        let state = new_timer_state();
        let handler = TimerHandler::new(state.clone(), crate::db::open_test_database());

        let result = handler
            .execute(&crate::action_registry::ExecContext::default(), "stopwatch")
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(body(&result), Some("__timer_panel__"));

        let timers = get_all_timers(&state);
        assert_eq!(timers.len(), 1);
        assert_eq!(timers[0].name, "Stopwatch");
        assert!(timers[0].stopwatch);
        assert_eq!(timers[0].duration_secs, 0);
        assert!(!timers[0].done); // stopwatches are never "done"
    }

    #[tokio::test]
    async fn test_stopwatch_named() {
        let state = new_timer_state();
        let handler = TimerHandler::new(state.clone(), crate::db::open_test_database());

        let result = handler
            .execute(
                &crate::action_registry::ExecContext::default(),
                "stopwatch workout",
            )
            .await
            .unwrap();
        assert!(result.success);

        let timers = get_all_timers(&state);
        assert_eq!(timers.len(), 1);
        assert_eq!(timers[0].name, "workout");
        assert!(timers[0].stopwatch);
    }

    #[tokio::test]
    async fn test_stopwatch_stop() {
        let state = new_timer_state();
        let handler = TimerHandler::new(state.clone(), crate::db::open_test_database());

        handler
            .execute(
                &crate::action_registry::ExecContext::default(),
                "stopwatch run",
            )
            .await
            .unwrap();

        let result = handler
            .execute(
                &crate::action_registry::ExecContext::default(),
                "stopwatch stop run",
            )
            .await
            .unwrap();
        assert!(result.success);
        assert!(get_all_timers(&state).is_empty());
    }

    #[tokio::test]
    async fn test_stopwatch_sw_alias() {
        let state = new_timer_state();
        let handler = TimerHandler::new(state.clone(), crate::db::open_test_database());

        let result = handler
            .execute(&crate::action_registry::ExecContext::default(), "sw")
            .await
            .unwrap();
        assert!(result.success);

        let timers = get_all_timers(&state);
        assert_eq!(timers.len(), 1);
        assert!(timers[0].stopwatch);
    }
}

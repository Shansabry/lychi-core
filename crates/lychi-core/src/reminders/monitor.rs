use std::sync::Arc;

use redb::Database;

use crate::reminders::store::RemindersStore;

/// Check for due reminders and fire desktop notifications.
///
/// Called from the timer monitor loop (single thread for all notify-rust calls)
/// to avoid concurrent D-Bus access that causes heap corruption.
pub fn check_and_fire(store: &RemindersStore, db: &Arc<Database>) {
    match store.get_pending(db) {
        Ok(pending) => {
            for (id, entry) in pending {
                if let Err(e) = notify_rust::Notification::new()
                    .summary("Lychi Reminder")
                    .body(&entry.text)
                    .icon("alarm-symbolic")
                    .timeout(notify_rust::Timeout::Milliseconds(15000))
                    .show()
                {
                    tracing::warn!("[reminder] notification error: {e}");
                }
                // The reminder TEXT is user content — keep it out of the
                // default-level (shareable) log file.
                tracing::info!("[reminder] fired: {id}");
                tracing::debug!("[reminder] text: {}", entry.text);

                if let Err(e) = store.mark_fired(db, &id) {
                    tracing::error!("[reminder] failed to mark fired {id}: {e}");
                }
            }
        }
        Err(e) => {
            tracing::error!("[reminder] failed to check pending: {e}");
        }
    }
}

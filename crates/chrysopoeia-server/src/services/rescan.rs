//! Periodic rescans, every `rescan_interval_hours` per library.

use std::collections::HashMap;
use std::time::Duration;

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::db;
use crate::services::library;
use crate::state::AppState;

/// How often the schedule is checked.
const CHECK_EVERY: Duration = Duration::from_secs(60);

/// Whether a library is due for a rescan.
pub fn is_due(last: Option<DateTime<Utc>>, now: DateTime<Utc>, interval_hours: u32) -> bool {
    if interval_hours == 0 {
        return false;
    }
    last.is_none_or(|t| now - t >= chrono::Duration::hours(i64::from(interval_hours)))
}

/// The rescan loop. Returns when the server shuts down.
pub async fn run(state: AppState) {
    // Last time a scan was started (or found already running) per library,
    // so a library whose scan keeps failing is retried at the interval, not
    // every minute.
    let mut attempts: HashMap<Uuid, DateTime<Utc>> = HashMap::new();
    let mut tick = tokio::time::interval_at(tokio::time::Instant::now() + CHECK_EVERY, CHECK_EVERY);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            () = state.shutdown.cancelled() => break,
            _ = tick.tick() => {}
        }
        let hours = state.settings().rescan_interval_hours;
        if hours == 0 {
            continue;
        }
        let libs = match db::libraries::list(state.db.pool()).await {
            Ok(l) => l,
            Err(e) => {
                tracing::error!("could not list libraries for rescans: {e}");
                continue;
            }
        };
        let now = Utc::now();
        for lib in libs.into_iter().filter(|l| l.enabled) {
            let last = match (lib.last_scan_at, attempts.get(&lib.id).copied()) {
                (Some(a), Some(b)) => Some(a.max(b)),
                (a, b) => a.or(b),
            };
            if is_due(last, now, hours) {
                attempts.insert(lib.id, now);
                if library::start_scan(&state, lib.id).is_ok() {
                    tracing::info!(library = %lib.name, "periodic rescan started");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn due_rules() {
        let now = Utc::now();
        assert!(is_due(None, now, 12));
        assert!(!is_due(None, now, 0));
        assert!(!is_due(Some(now - chrono::Duration::hours(1)), now, 12));
        assert!(is_due(Some(now - chrono::Duration::hours(13)), now, 12));
    }
}

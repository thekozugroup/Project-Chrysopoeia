//! The `settings` key/value table: the global [`Settings`] JSON plus a few
//! server flags.

use sqlx::SqlitePool;
use szalinski_core::Settings;

/// Key of the settings JSON row.
pub const SETTINGS_KEY: &str = "settings";
/// Key of the persisted "queue paused" flag.
pub const QUEUE_PAUSED_KEY: &str = "queue_paused";
/// Key of the "last shutdown was clean" flag.
pub const CLEAN_SHUTDOWN_KEY: &str = "clean_shutdown";
/// Key of the `HW_ACCEL` value last applied to the settings.
pub const HW_ACCEL_APPLIED_KEY: &str = "hw_accel_applied";

/// Read a raw value.
pub async fn get_raw(pool: &SqlitePool, key: &str) -> sqlx::Result<Option<String>> {
    sqlx::query_scalar("SELECT value FROM settings WHERE key = ?")
        .bind(key)
        .fetch_optional(pool)
        .await
}

/// Write a raw value.
pub async fn set_raw(pool: &SqlitePool, key: &str, value: &str) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO settings (key, value) VALUES (?, ?) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )
    .bind(key)
    .bind(value)
    .execute(pool)
    .await?;
    Ok(())
}

/// Load the saved settings. `None` on first run. A row that no longer parses
/// (hand-edited, or from an incompatible version) is logged and replaced by
/// defaults rather than blocking start-up.
pub async fn load(pool: &SqlitePool) -> sqlx::Result<Option<Settings>> {
    let Some(raw) = get_raw(pool, SETTINGS_KEY).await? else {
        return Ok(None);
    };
    match serde_json::from_str::<Settings>(&raw) {
        Ok(s) => Ok(Some(s)),
        Err(e) => {
            tracing::warn!("saved settings could not be read ({e}); using defaults");
            Ok(Some(Settings::default()))
        }
    }
}

/// Save the settings.
pub async fn save(pool: &SqlitePool, settings: &Settings) -> sqlx::Result<()> {
    let json = super::to_json(settings)?;
    set_raw(pool, SETTINGS_KEY, &json).await
}

/// Read a boolean flag.
pub async fn get_flag(pool: &SqlitePool, key: &str) -> sqlx::Result<Option<bool>> {
    Ok(get_raw(pool, key).await?.map(|v| v == "true"))
}

/// Write a boolean flag.
pub async fn set_flag(pool: &SqlitePool, key: &str, value: bool) -> sqlx::Result<()> {
    set_raw(pool, key, if value { "true" } else { "false" }).await
}

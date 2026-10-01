//! The `libraries` table.

use chrono::{DateTime, Utc};
use chrysopoeia_core::TranscodeProfile;
use sqlx::sqlite::SqliteRow;
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use super::{opt_ts_col, parse_json, to_json, ts, ts_col, uuid_col};

/// A stored library, without the live fields (`stats`, `scanning`,
/// `path_error`) that `services::library::view` adds.
#[derive(Debug, Clone, PartialEq)]
pub struct LibraryRow {
    pub id: Uuid,
    pub name: String,
    pub path: String,
    pub enabled: bool,
    pub profile: TranscodeProfile,
    pub last_scan_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

const COLUMNS: &str = "id, name, path, enabled, profile, last_scan_at, created_at";

fn from_row(row: &SqliteRow) -> sqlx::Result<LibraryRow> {
    let profile: String = row.try_get("profile")?;
    Ok(LibraryRow {
        id: uuid_col(row, "id")?,
        name: row.try_get("name")?,
        path: row.try_get("path")?,
        enabled: row.try_get::<i64, _>("enabled")? != 0,
        profile: parse_json(&profile)?,
        last_scan_at: opt_ts_col(row, "last_scan_at")?,
        created_at: ts_col(row, "created_at")?,
    })
}

/// All libraries, by name.
pub async fn list(pool: &SqlitePool) -> sqlx::Result<Vec<LibraryRow>> {
    let rows = sqlx::query(&format!(
        "SELECT {COLUMNS} FROM libraries ORDER BY name COLLATE NOCASE, path"
    ))
    .fetch_all(pool)
    .await?;
    rows.iter().map(from_row).collect()
}

/// One library.
pub async fn get(pool: &SqlitePool, id: Uuid) -> sqlx::Result<Option<LibraryRow>> {
    let row = sqlx::query(&format!("SELECT {COLUMNS} FROM libraries WHERE id = ?"))
        .bind(id.to_string())
        .fetch_optional(pool)
        .await?;
    row.as_ref().map(from_row).transpose()
}

/// Insert a library.
pub async fn insert(pool: &SqlitePool, lib: &LibraryRow) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO libraries (id, name, path, enabled, profile, last_scan_at, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(lib.id.to_string())
    .bind(&lib.name)
    .bind(&lib.path)
    .bind(i64::from(lib.enabled))
    .bind(to_json(&lib.profile)?)
    .bind(lib.last_scan_at.map(ts))
    .bind(ts(lib.created_at))
    .execute(pool)
    .await?;
    Ok(())
}

/// Save name, enabled and profile.
pub async fn update(pool: &SqlitePool, lib: &LibraryRow) -> sqlx::Result<()> {
    sqlx::query("UPDATE libraries SET name = ?, enabled = ?, profile = ? WHERE id = ?")
        .bind(&lib.name)
        .bind(i64::from(lib.enabled))
        .bind(to_json(&lib.profile)?)
        .bind(lib.id.to_string())
        .execute(pool)
        .await?;
    Ok(())
}

/// Record a finished scan.
pub async fn set_last_scan(pool: &SqlitePool, id: Uuid, at: DateTime<Utc>) -> sqlx::Result<()> {
    sqlx::query("UPDATE libraries SET last_scan_at = ? WHERE id = ?")
        .bind(ts(at))
        .bind(id.to_string())
        .execute(pool)
        .await?;
    Ok(())
}

/// Libraries whose last scan left files for later because they were still
/// being copied.
pub async fn with_settling_files(pool: &SqlitePool) -> sqlx::Result<Vec<Uuid>> {
    let rows = sqlx::query("SELECT id FROM libraries WHERE settling > 0")
        .fetch_all(pool)
        .await?;
    rows.iter().map(|r| uuid_col(r, "id")).collect()
}

/// Delete a library; its files, jobs and savings history cascade. Returns
/// whether it existed.
pub async fn delete(pool: &SqlitePool, id: Uuid) -> sqlx::Result<bool> {
    let done = sqlx::query("DELETE FROM libraries WHERE id = ?")
        .bind(id.to_string())
        .execute(pool)
        .await?;
    Ok(done.rows_affected() > 0)
}

/// Drives and shares mounted inside a library folder that scans have seen
/// (see `services::library::mounts`).
pub async fn mounts(pool: &SqlitePool, id: Uuid) -> sqlx::Result<Vec<String>> {
    sqlx::query_scalar("SELECT path FROM library_mounts WHERE library_id = ? ORDER BY path")
        .bind(id.to_string())
        .fetch_all(pool)
        .await
}

/// Replace a library's known mounts.
pub async fn set_mounts(
    conn: &mut sqlx::SqliteConnection,
    id: Uuid,
    paths: &[String],
) -> sqlx::Result<()> {
    sqlx::query("DELETE FROM library_mounts WHERE library_id = ?")
        .bind(id.to_string())
        .execute(&mut *conn)
        .await?;
    for path in paths {
        sqlx::query("INSERT OR IGNORE INTO library_mounts (library_id, path) VALUES (?, ?)")
            .bind(id.to_string())
            .bind(path)
            .execute(&mut *conn)
            .await?;
    }
    Ok(())
}

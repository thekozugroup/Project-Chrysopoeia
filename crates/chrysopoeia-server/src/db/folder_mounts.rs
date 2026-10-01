//! The mount points the folders Chrysopoeia uses were seen on or under
//! (see `services::share_mounts`).

use sqlx::SqlitePool;

/// The mount points remembered for `folder`.
pub async fn get(pool: &SqlitePool, folder: &str) -> sqlx::Result<Vec<String>> {
    sqlx::query_scalar("SELECT mount FROM folder_mounts WHERE folder = ? ORDER BY mount")
        .bind(folder)
        .fetch_all(pool)
        .await
}

/// Remember `mounts` for `folder` too.
pub async fn add(
    conn: &mut sqlx::SqliteConnection,
    folder: &str,
    mounts: &[String],
) -> sqlx::Result<()> {
    for mount in mounts {
        sqlx::query("INSERT OR IGNORE INTO folder_mounts (folder, mount) VALUES (?, ?)")
            .bind(folder)
            .bind(mount)
            .execute(&mut *conn)
            .await?;
    }
    Ok(())
}

/// Forget the mount points of `folder`.
pub async fn forget(conn: &mut sqlx::SqliteConnection, folder: &str) -> sqlx::Result<()> {
    sqlx::query("DELETE FROM folder_mounts WHERE folder = ?")
        .bind(folder)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

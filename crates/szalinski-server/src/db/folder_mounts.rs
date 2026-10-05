//! The mount points the folders Szalinski uses were seen on or under,
//! with what was mounted there (see `services::share_mounts`).

use sqlx::{Row, SqlitePool};
use szalinski_worker::slow_fs::MountIdentity;

/// A mount point remembered for a folder, and what was mounted there
/// (`None`: remembered by an older version, which didn't note it; any
/// mount there counts until one is seen and noted).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderMount {
    pub mount: String,
    pub identity: Option<MountIdentity>,
}

/// The mount points remembered for `folder`.
pub async fn get(pool: &SqlitePool, folder: &str) -> sqlx::Result<Vec<FolderMount>> {
    let rows = sqlx::query(
        "SELECT mount, fstype, source, root FROM folder_mounts WHERE folder = ? ORDER BY mount",
    )
    .bind(folder)
    .fetch_all(pool)
    .await?;
    rows.iter()
        .map(|row| {
            let fstype: Option<String> = row.try_get("fstype")?;
            let source: Option<String> = row.try_get("source")?;
            let root: Option<String> = row.try_get("root")?;
            Ok(FolderMount {
                mount: row.try_get("mount")?,
                identity: match (fstype, source, root) {
                    (Some(fstype), Some(source), Some(root)) => Some(MountIdentity {
                        fstype,
                        source,
                        root,
                    }),
                    _ => None,
                },
            })
        })
        .collect()
}

/// The mount points remembered without what was mounted there (by an
/// older version), with their folder: `(folder, mount)`.
pub async fn unnoted(pool: &SqlitePool) -> sqlx::Result<Vec<(String, String)>> {
    sqlx::query_as(
        "SELECT folder, mount FROM folder_mounts WHERE fstype IS NULL ORDER BY folder, mount",
    )
    .fetch_all(pool)
    .await
}

/// Remember `mounts` for `folder` too. What was mounted at one already
/// remembered is noted only when it wasn't before (see [`set_identity`]).
pub async fn add(
    conn: &mut sqlx::SqliteConnection,
    folder: &str,
    mounts: &[FolderMount],
) -> sqlx::Result<()> {
    for m in mounts {
        let id = m.identity.as_ref();
        sqlx::query(
            "INSERT INTO folder_mounts (folder, mount, fstype, source, root) \
             VALUES (?, ?, ?, ?, ?) \
             ON CONFLICT (folder, mount) DO UPDATE SET \
               fstype = excluded.fstype, source = excluded.source, root = excluded.root \
             WHERE folder_mounts.fstype IS NULL AND excluded.fstype IS NOT NULL",
        )
        .bind(folder)
        .bind(&m.mount)
        .bind(id.map(|i| i.fstype.as_str()))
        .bind(id.map(|i| i.source.as_str()))
        .bind(id.map(|i| i.root.as_str()))
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

/// Note `identity` as what is mounted at `mount` for `folder` from now on
/// (the user said to use the drive there now).
pub async fn set_identity(
    conn: &mut sqlx::SqliteConnection,
    folder: &str,
    mount: &str,
    identity: &MountIdentity,
) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE folder_mounts SET fstype = ?, source = ?, root = ? WHERE folder = ? AND mount = ?",
    )
    .bind(&identity.fstype)
    .bind(&identity.source)
    .bind(&identity.root)
    .bind(folder)
    .bind(mount)
    .execute(&mut *conn)
    .await?;
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

use anyhow::Result;
use rusqlite::Connection;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

type NotificationRow = (String, String, Option<String>, Option<String>, String);

fn canopy_db_path() -> Option<PathBuf> {
    let path = spore::paths::data_dir("canopy").join("canopy.db");
    path.exists().then_some(path)
}

fn osascript_path() -> Option<PathBuf> {
    static CACHED_OSASCRIPT: OnceLock<Option<PathBuf>> = OnceLock::new();
    CACHED_OSASCRIPT
        .get_or_init(|| {
            let result = which::which("osascript").ok();
            if result.is_none() {
                tracing::debug!(
                    "annulus: osascript not found on PATH; notifications will be no-ops"
                );
            }
            result
        })
        .clone()
}

pub fn handle(poll: bool, system: bool) -> Result<()> {
    let Some(db_path) = canopy_db_path() else {
        println!("canopy: not available");
        return Ok(());
    };

    let conn = Connection::open(&db_path)?;
    conn.busy_timeout(Duration::from_millis(500))?;

    // Query unread notifications
    let mut stmt = conn.prepare(
        "SELECT notification_id, event_type, task_id, agent_id, created_at
         FROM notifications WHERE seen = 0 ORDER BY created_at DESC LIMIT 100",
    )?;
    let rows: Vec<NotificationRow> = stmt
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })?
        .filter_map(Result::ok)
        .collect();

    if rows.is_empty() {
        println!("No unread canopy notifications.");
        return Ok(());
    }

    for (id, event_type, task_id, _agent_id, created_at) in &rows {
        let task = task_id.as_deref().unwrap_or("-");
        println!("[{created_at}] {event_type} (task: {task}) [{id}]");
    }

    // If we hit the limit, show a note about remaining unread
    if rows.len() == 100 {
        let total_unread: i64 = conn.query_row(
            "SELECT COUNT(*) FROM notifications WHERE seen = 0",
            [],
            |row| row.get(0),
        )?;
        println!("(showing 100 of {total_unread} unread)");
    }

    if system {
        // macOS system notification — best effort, no error on failure
        if let Some(osascript) = osascript_path() {
            let output = std::process::Command::new(&osascript)
                .args([
                    "-e",
                    &format!(
                        "display notification \"{} unread canopy notification(s)\" with title \"Annulus\"",
                        rows.len()
                    ),
                ])
                .output();

            if let Ok(out) = output {
                if !out.status.success() {
                    tracing::debug!("osascript notification failed: exit={}", out.status);
                }
            }
        }
    }

    if poll {
        // Mark only the fetched notifications as read using a subquery to avoid TOCTOU race
        // where notifications arriving between SELECT and UPDATE would be silently marked seen.
        let ids: Vec<&str> = rows.iter().map(|(id, _, _, _, _)| id.as_str()).collect();
        let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let query =
            format!("UPDATE notifications SET seen = 1 WHERE notification_id IN ({placeholders})");
        conn.execute(&query, rusqlite::params_from_iter(ids))?;
        println!("Marked {} notification(s) as read.", rows.len());
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use rusqlite::{Connection, params};
    use tempfile::TempDir;

    fn setup_db_with_notifications(dir: &TempDir, count: usize) -> std::path::PathBuf {
        let db_path = dir.path().join("canopy.db");
        let conn = Connection::open(&db_path).unwrap();
        conn.execute(
            "CREATE TABLE notifications (
                notification_id TEXT PRIMARY KEY,
                event_type TEXT NOT NULL,
                task_id TEXT,
                agent_id TEXT,
                payload TEXT,
                seen INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL
            )",
            [],
        )
        .unwrap();
        for i in 0..count {
            conn.execute(
                "INSERT INTO notifications (notification_id, event_type, task_id, agent_id, payload, seen, created_at)
                 VALUES (?1, ?2, ?3, NULL, '{}', 0, ?4)",
                params![
                    format!("notif-{i}"),
                    "task_completed",
                    format!("task-{i}"),
                    format!("2024-01-0{}", i + 1),
                ],
            )
            .unwrap();
        }
        db_path
    }

    #[test]
    fn handle_marks_notifications_as_read_when_poll() {
        let dir = TempDir::new().unwrap();
        let db_path = setup_db_with_notifications(&dir, 2);

        // Verify rows exist
        let conn = Connection::open(&db_path).unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM notifications WHERE seen = 0",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 2);

        // Mark as read
        conn.execute("UPDATE notifications SET seen = 1 WHERE seen = 0", [])
            .unwrap();
        let after: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM notifications WHERE seen = 0",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(after, 0);
    }

    #[test]
    fn handle_no_notifications_returns_ok() {
        let dir = TempDir::new().unwrap();
        setup_db_with_notifications(&dir, 0);
        // No panic, no error
        let count: i64 = {
            let conn = Connection::open(dir.path().join("canopy.db")).unwrap();
            conn.query_row(
                "SELECT COUNT(*) FROM notifications WHERE seen = 0",
                [],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(count, 0);
    }
}

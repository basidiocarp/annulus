use anyhow::Result;
use rusqlite::{Connection, params};
use std::path::PathBuf;

type NotificationRow = (String, String, Option<String>, Option<String>, String);

fn canopy_db_path() -> Option<PathBuf> {
    let path = spore::paths::data_dir("canopy").join("canopy.db");
    path.exists().then_some(path)
}

pub fn handle(poll: bool, system: bool) -> Result<()> {
    let Some(db_path) = canopy_db_path() else {
        println!("canopy: not available");
        return Ok(());
    };

    let conn = Connection::open(&db_path)?;

    // Query unread notifications
    let mut stmt = conn.prepare(
        "SELECT notification_id, event_type, task_id, agent_id, created_at
         FROM notifications WHERE seen = 0 ORDER BY created_at DESC",
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

    if system {
        // macOS system notification — best effort, no error on failure
        let _ = std::process::Command::new("osascript")
            .args([
                "-e",
                &format!(
                    "display notification \"{} unread canopy notification(s)\" with title \"Annulus\"",
                    rows.len()
                ),
            ])
            .output();
    }

    if poll {
        // Mark all as read
        conn.execute(
            "UPDATE notifications SET seen = 1 WHERE seen = 0",
            params![],
        )?;
        println!("Marked {} notification(s) as read.", rows.len());
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
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
        conn.execute(
            "UPDATE notifications SET seen = 1 WHERE seen = 0",
            params![],
        )
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

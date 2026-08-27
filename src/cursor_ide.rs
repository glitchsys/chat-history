//! Cursor IDE chats from `state.vscdb` (SQLite).
//! Schema is unofficial and can drift with Cursor releases.

use crate::parser::{clean_first_prompt, extract_text, is_clear_metadata, is_warmup_message};
use crate::session::{Message, Session, user_home};
use chrono::{TimeZone, Utc};
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;
use std::path::{Path, PathBuf};

pub fn cursor_user_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("CURSOR_USER_DIR")
        && !dir.is_empty()
    {
        return PathBuf::from(dir);
    }
    let home = user_home().unwrap_or_else(|| PathBuf::from("."));
    if cfg!(target_os = "macos") {
        home.join("Library/Application Support/Cursor/User")
    } else if cfg!(windows) {
        std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or(home)
            .join("Cursor/User")
    } else {
        home.join(".config/Cursor/User")
    }
}

fn global_vscdb() -> PathBuf {
    cursor_user_dir().join("globalStorage/state.vscdb")
}

fn open_ro(path: &Path) -> Option<Connection> {
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI;
    let uri = format!("file:{}?mode=ro", path.display());
    Connection::open_with_flags(&uri, flags).ok()
}

fn ms_iso_date(ms: i64) -> (String, String) {
    let Some(dt) = Utc.timestamp_millis_opt(ms).single() else {
        return (String::new(), String::new());
    };
    (
        dt.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        dt.format("%Y-%m-%d").to_string(),
    )
}

fn workspace_path(value: &Value) -> String {
    value
        .pointer("/workspaceIdentifier/uri/fsPath")
        .or_else(|| value.pointer("/workspaceIdentifier/uri/path"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn blob_text(value: rusqlite::types::ValueRef<'_>) -> String {
    match value {
        rusqlite::types::ValueRef::Text(t) => String::from_utf8_lossy(t).into_owned(),
        rusqlite::types::ValueRef::Blob(b) => String::from_utf8_lossy(b).into_owned(),
        _ => String::new(),
    }
}

fn first_user_text(conn: &Connection, composer_id: &str) -> String {
    let pattern = format!("bubbleId:{composer_id}:%");
    let mut stmt = match conn.prepare("SELECT value FROM cursorDiskKV WHERE key LIKE ?1") {
        Ok(s) => s,
        Err(_) => return String::new(),
    };
    let rows = stmt.query_map([&pattern], |row| Ok(blob_text(row.get_ref(0)?)));
    let Ok(rows) = rows else {
        return String::new();
    };
    for val in rows.flatten() {
        let Ok(entry) = serde_json::from_str::<Value>(&val) else {
            continue;
        };
        if entry.get("type").and_then(Value::as_i64) != Some(1) {
            continue;
        }
        let text = entry
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let cleaned = clean_first_prompt(&text);
        if !cleaned.is_empty() && !is_warmup_message(&cleaned) && !is_clear_metadata(&cleaned) {
            return cleaned.chars().take(300).collect();
        }
    }
    String::new()
}

pub fn load_cursor_ide_sessions() -> Vec<Session> {
    let db = global_vscdb();
    if !db.exists() {
        return Vec::new();
    }
    let Some(conn) = open_ro(&db) else {
        return Vec::new();
    };
    let mut sessions = Vec::new();
    let sql = "SELECT composerId, createdAt, lastUpdatedAt, isSubagent, value FROM composerHeaders";
    let mut stmt = match conn.prepare(sql) {
        Ok(s) => s,
        Err(_) => return sessions,
    };
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<i64>>(1)?.unwrap_or(0),
            row.get::<_, Option<i64>>(2)?.unwrap_or(0),
            row.get::<_, i64>(3).unwrap_or(0) != 0,
            blob_text(row.get_ref(4)?),
        ))
    });
    let Ok(rows) = rows else {
        return sessions;
    };
    let file = db.to_string_lossy().to_string();
    for row in rows.flatten() {
        let (id, created_ms, updated_ms, is_sidechain, raw) = row;
        let meta: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
        let name = meta
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        let subtitle = meta
            .get("subtitle")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        let mut first: String = if !subtitle.is_empty() {
            subtitle.chars().take(300).collect()
        } else {
            name.chars().take(300).collect()
        };
        if first.is_empty() {
            first = first_user_text(&conn, &id);
        }
        let ts = if updated_ms > 0 { updated_ms } else { created_ms };
        let (iso, date) = ms_iso_date(ts);
        sessions.push(Session {
            source: "cursor".into(),
            id,
            summary: name,
            first_prompt: first,
            created: iso.clone(),
            modified: iso,
            date,
            messages: 0,
            branch: String::new(),
            project: workspace_path(&meta),
            file: file.clone(),
            is_sidechain,
        });
    }
    sessions
}

pub fn parse_cursor_ide(session: &Session) -> Vec<Message> {
    let db = Path::new(&session.file);
    let Some(conn) = open_ro(db) else {
        return Vec::new();
    };
    let pattern = format!("bubbleId:{}:%", session.id);
    let mut stmt = match conn.prepare("SELECT value FROM cursorDiskKV WHERE key LIKE ?1") {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let rows = stmt.query_map([&pattern], |row| Ok(blob_text(row.get_ref(0)?)));
    let Ok(rows) = rows else {
        return Vec::new();
    };
    let mut messages = Vec::new();
    let mut total_chars: usize = 0;
    for val in rows.flatten() {
        if total_chars > 4 * 1024 * 1024 {
            break;
        }
        let Ok(entry) = serde_json::from_str::<Value>(&val) else {
            continue;
        };
        let ty = entry.get("type").and_then(Value::as_i64).unwrap_or(0);
        let role = match ty {
            1 => "user",
            2 => "assistant",
            _ => continue,
        };
        let text = entry
            .get("text")
            .and_then(Value::as_str)
            .map(|s| s.to_string())
            .unwrap_or_else(|| {
                entry
                    .get("richText")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string()
            });
        let cleaned = if role == "user" {
            crate::parser::clean_prompt(&text)
        } else {
            extract_text(&Value::String(text.clone()))
        };
        if cleaned.is_empty() {
            continue;
        }
        if role == "user" && (is_warmup_message(&cleaned) || is_clear_metadata(&cleaned)) {
            continue;
        }
        total_chars += cleaned.len();
        let ts = entry
            .get("createdAt")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        messages.push(Message {
            uuid: entry
                .get("bubbleId")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            timestamp: ts,
            role: role.into(),
            content: cleaned,
            session_id: session.id.clone(),
            project_path: session.project.clone(),
            tool_uses: Vec::new(),
            files_referenced: Vec::new(),
            error_patterns: Vec::new(),
            relevance_score: 0.0,
            final_score: 0.0,
        });
    }
    messages
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn fixture_db() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::TempDir::new().unwrap();
        let user = tmp.path().join("User");
        let dir = user.join("globalStorage");
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("state.vscdb");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE composerHeaders (
                composerId TEXT PRIMARY KEY,
                workspaceId TEXT,
                createdAt INTEGER,
                lastUpdatedAt INTEGER,
                isArchived INTEGER,
                isSubagent INTEGER,
                recency INTEGER,
                checkpointAt INTEGER,
                value TEXT
            );
            CREATE TABLE cursorDiskKV (key TEXT UNIQUE, value BLOB);
            "#,
        )
        .unwrap();
        let header = serde_json::json!({
            "name": "Explain the error handler",
            "unifiedMode": "agent",
            "subtitle": "Read src/main.rs",
            "workspaceIdentifier": {"uri": {"fsPath": "/home/alice/src/myapp", "path": "/home/alice/src/myapp"}}
        });
        conn.execute(
            "INSERT INTO composerHeaders (composerId, createdAt, lastUpdatedAt, isSubagent, value)
             VALUES (?1, 1782941109570, 1782941109570, 0, ?2)",
            rusqlite::params!["aaaa1111-bbbb-cccc-dddd-eeeeeeeeeeee", header.to_string()],
        )
        .unwrap();
        let bubble = serde_json::json!({
            "type": 1,
            "text": "How does the error handler work?",
            "bubbleId": "b1",
            "createdAt": "2026-07-01T21:25:09.594Z"
        });
        conn.execute(
            "INSERT INTO cursorDiskKV (key, value) VALUES (?1, ?2)",
            rusqlite::params![
                "bubbleId:aaaa1111-bbbb-cccc-dddd-eeeeeeeeeeee:b1",
                bubble.to_string()
            ],
        )
        .unwrap();
        drop(conn);
        (tmp, user)
    }

    #[test]
    fn loads_ide_chat_from_vscdb() {
        let (_tmp, user) = fixture_db();
        unsafe { std::env::set_var("CURSOR_USER_DIR", user.to_str().unwrap()) };
        let sessions = load_cursor_ide_sessions();
        unsafe { std::env::remove_var("CURSOR_USER_DIR") };
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].source, "cursor");
        assert_eq!(sessions[0].summary, "Explain the error handler");
        assert!(
            sessions[0].first_prompt.contains("error handler")
                || sessions[0].first_prompt.contains("main.rs")
        );
        assert_eq!(sessions[0].project, "/home/alice/src/myapp");
        let msgs = parse_cursor_ide(&sessions[0]);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, "user");
    }
}

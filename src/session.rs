use crate::parser::{clean_prompt, extract_text, is_clear_metadata, is_warmup_message};
use chrono::{DateTime, FixedOffset, NaiveDate, Utc};
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashSet;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Clone, Debug)]
pub struct Session {
    pub source: String,
    pub id: String,
    pub summary: String,
    pub first_prompt: String,
    pub created: String,
    pub modified: String,
    pub date: String,
    pub messages: u64,
    pub branch: String,
    pub project: String,
    pub file: String,
    #[allow(dead_code)]
    pub is_sidechain: bool,
}

#[derive(Clone, Debug)]
pub struct Message {
    #[allow(dead_code)]
    pub uuid: String,
    pub timestamp: String,
    pub role: String,
    pub content: String,
    pub session_id: String,
    pub project_path: String,
    pub tool_uses: Vec<String>,
    pub files_referenced: Vec<String>,
    pub error_patterns: Vec<String>,
    pub relevance_score: f64,
    pub final_score: f64,
}

impl Message {
    pub fn content_lower(&self) -> String {
        self.content.to_lowercase()
    }
}

pub struct SessionMeta {
    pub summary: Option<String>,
    pub custom_title: Option<String>,
    pub model: Option<String>,
    pub total_tokens: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct IndexFile {
    #[serde(default)]
    entries: Vec<IndexEntry>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct IndexEntry {
    #[serde(default)]
    session_id: String,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    first_prompt: String,
    #[serde(default)]
    created: String,
    #[serde(default)]
    modified: String,
    #[serde(default)]
    message_count: u64,
    #[serde(default)]
    git_branch: String,
    #[serde(default)]
    project_path: String,
    #[serde(default)]
    full_path: String,
    #[serde(default)]
    is_sidechain: bool,
}

fn home_dir() -> PathBuf {
    dirs_next().unwrap_or_else(|| PathBuf::from("."))
}

fn dirs_next() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

pub fn claude_projects_dir() -> PathBuf {
    if let Ok(config_dir) = std::env::var("CLAUDE_CONFIG_DIR") {
        return PathBuf::from(config_dir).join("projects");
    }
    home_dir().join(".claude").join("projects")
}

fn cursor_projects_dir() -> PathBuf {
    home_dir().join(".cursor").join("projects")
}

pub fn codex_home() -> PathBuf {
    if let Some(dir) = std::env::var_os("CODEX_HOME") {
        return PathBuf::from(dir);
    }
    home_dir().join(".codex")
}

fn codex_sessions_dir() -> PathBuf {
    codex_home().join("sessions")
}

fn read_cwd_from_jsonl(path: &Path) -> Option<String> {
    let file = fs::File::open(path).ok()?;
    let reader = BufReader::new(file);
    for line in reader.lines().take(10) {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let entry: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if let Some(cwd) = entry.get("cwd").and_then(Value::as_str)
            && !cwd.is_empty()
        {
            return Some(cwd.to_string());
        }
    }
    None
}

pub fn encode_path_for_claude(path: &Path) -> String {
    path.to_string_lossy().replace('/', "-")
}

fn normalize_for_project_match(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect::<String>()
        .to_lowercase()
}

pub fn copy_session_to_dir(session: &Session, target_dir: &Path) -> std::io::Result<()> {
    fs::create_dir_all(target_dir)?;

    let src = Path::new(&session.file);
    let filename = src.file_name().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("session file path '{}' has no filename", session.file),
        )
    })?;
    fs::copy(src, target_dir.join(filename))?;

    if let Some(parent) = src.parent() {
        let companion = parent.join(&session.id);
        if companion.is_dir() {
            copy_dir_recursive(&companion, &target_dir.join(&session.id))?;
        }
    }
    Ok(())
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let dest_path = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_recursive(&entry.path(), &dest_path)?;
        } else {
            fs::copy(entry.path(), &dest_path)?;
        }
    }
    Ok(())
}

fn mtime_iso(path: &Path) -> Option<String> {
    let meta = fs::metadata(path).ok()?;
    let mtime = meta.modified().ok()?;
    let dur = mtime.duration_since(SystemTime::UNIX_EPOCH).ok()?;
    let dt = DateTime::<Utc>::from_timestamp(dur.as_secs() as i64, 0)?;
    Some(dt.format("%Y-%m-%dT%H:%M:%SZ").to_string())
}

pub fn parse_any_timestamp(s: &str) -> Option<DateTime<FixedOffset>> {
    let s = s.replace('Z', "+00:00");
    DateTime::parse_from_rfc3339(&s)
        .or_else(|_| DateTime::parse_from_str(&s, "%Y-%m-%dT%H:%M:%S%.f%:z"))
        .ok()
        .or_else(|| s.parse::<DateTime<Utc>>().ok().map(|t| t.fixed_offset()))
}

fn mtime_date(path: &Path) -> Option<String> {
    let meta = fs::metadata(path).ok()?;
    let mtime = meta.modified().ok()?;
    let dur = mtime.duration_since(SystemTime::UNIX_EPOCH).ok()?;
    let dt = DateTime::<Utc>::from_timestamp(dur.as_secs() as i64, 0)?;
    Some(dt.format("%Y-%m-%d").to_string())
}

fn claude_first_prompt(path: &Path) -> String {
    let file = match fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return String::new(),
    };
    let reader = BufReader::new(file);
    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        let trimmed = line.trim().to_string();
        if trimmed.is_empty() {
            continue;
        }
        let entry: Value = match serde_json::from_str(&trimmed) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if entry.get("type").and_then(Value::as_str) != Some("user") {
            continue;
        }
        let content = entry
            .get("message")
            .and_then(|m| m.get("content"))
            .cloned()
            .unwrap_or(Value::String(String::new()));
        let text = extract_text(&content);
        let cleaned = clean_prompt(&text);
        if !cleaned.is_empty() && !is_warmup_message(&cleaned) && !is_clear_metadata(&cleaned) {
            return cleaned.chars().take(300).collect();
        }
    }
    String::new()
}

fn cursor_first_prompt_jsonl(path: &Path) -> String {
    let file = match fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return String::new(),
    };
    let reader = BufReader::new(file);
    if let Some(Ok(line)) = reader.lines().next()
        && let Ok(entry) = serde_json::from_str::<Value>(&line)
    {
        let content = entry
            .get("message")
            .and_then(|m| m.get("content"))
            .cloned()
            .unwrap_or(Value::String(String::new()));
        let text = extract_text(&content);
        return text.chars().take(300).collect();
    }
    String::new()
}

fn cursor_first_prompt_txt(path: &Path) -> String {
    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return String::new(),
    };
    for (i, line) in content.lines().enumerate() {
        if i == 0 {
            continue;
        }
        if i > 30 {
            break;
        }
        let s = line.trim();
        if !s.is_empty()
            && ![
                "<user_query>",
                "</user_query>",
                "user:",
                "assistant:",
                "<attached_files>",
            ]
            .contains(&s)
        {
            return s.chars().take(300).collect();
        }
    }
    String::new()
}

pub fn load_claude_sessions() -> Vec<Session> {
    let base = claude_projects_dir();
    if !base.exists() {
        return Vec::new();
    }
    let mut sessions = Vec::new();
    let mut indexed_ids = HashSet::new();

    for idx_path in glob::glob(&format!("{}/**/sessions-index.json", base.display()))
        .into_iter()
        .flatten()
        .flatten()
    {
        let data = match fs::read_to_string(&idx_path) {
            Ok(d) => d,
            Err(_) => continue,
        };
        let index: IndexFile = match serde_json::from_str(&data) {
            Ok(i) => i,
            Err(_) => continue,
        };
        for entry in index.entries {
            indexed_ids.insert(entry.session_id.clone());
            sessions.push(Session {
                source: "claude".into(),
                id: entry.session_id,
                summary: entry.summary,
                first_prompt: entry.first_prompt.chars().take(300).collect(),
                created: entry.created.clone(),
                modified: entry.modified,
                date: entry.created.get(..10).unwrap_or("").to_string(),
                messages: entry.message_count,
                branch: entry.git_branch,
                project: entry.project_path,
                file: entry.full_path,
                is_sidechain: entry.is_sidechain,
            });
        }
    }

    if let Ok(entries) = fs::read_dir(&base) {
        for entry in entries.flatten() {
            let dir = entry.path();
            if !dir.is_dir() {
                continue;
            }
            if let Ok(files) = fs::read_dir(&dir) {
                for f in files.flatten() {
                    let path = f.path();
                    if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                        continue;
                    }
                    let sid = path
                        .file_stem()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string();
                    if indexed_ids.contains(&sid) || sid.starts_with("agent-") {
                        continue;
                    }
                    let iso = mtime_iso(&path).unwrap_or_default();
                    let date = mtime_date(&path).unwrap_or_default();
                    let project = read_cwd_from_jsonl(&path).unwrap_or_else(|| {
                        dir.file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .replace('-', "/")
                    });
                    let first = claude_first_prompt(&path);
                    sessions.push(Session {
                        source: "claude".into(),
                        id: sid,
                        summary: String::new(),
                        first_prompt: first,
                        created: iso.clone(),
                        modified: iso,
                        date,
                        messages: 0,
                        branch: String::new(),
                        project,
                        file: path.to_string_lossy().to_string(),
                        is_sidechain: false,
                    });
                }
            }
        }
    }
    sessions
}

pub fn load_cursor_sessions() -> Vec<Session> {
    let base = cursor_projects_dir();
    if !base.exists() {
        return Vec::new();
    }
    let mut sessions = Vec::new();

    if let Ok(project_dirs) = fs::read_dir(&base) {
        for pd in project_dirs.flatten() {
            let transcripts = pd.path().join("agent-transcripts");
            if !transcripts.is_dir() {
                continue;
            }
            let mut txt_ids: HashSet<String> = HashSet::new();
            let mut dir_entries: Vec<(String, PathBuf)> = Vec::new();
            if let Ok(entries) = fs::read_dir(&transcripts) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().and_then(|e| e.to_str()) == Some("txt") {
                        let sid = path
                            .file_stem()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .to_string();
                        txt_ids.insert(sid.clone());
                        let jsonl_alt = transcripts.join(&sid).join(format!("{sid}.jsonl"));
                        let iso = mtime_iso(&path).unwrap_or_default();
                        let date = mtime_date(&path).unwrap_or_default();
                        let first = cursor_first_prompt_txt(&path);
                        let file = if jsonl_alt.exists() {
                            jsonl_alt.to_string_lossy().to_string()
                        } else {
                            path.to_string_lossy().to_string()
                        };
                        sessions.push(Session {
                            source: "cursor".into(),
                            id: sid,
                            summary: String::new(),
                            first_prompt: first,
                            created: iso.clone(),
                            modified: iso,
                            date,
                            messages: 0,
                            branch: String::new(),
                            project: pd.file_name().to_string_lossy().to_string(),
                            file,
                            is_sidechain: false,
                        });
                    } else if path.is_dir() {
                        let dirname = path
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .to_string();
                        dir_entries.push((dirname, path));
                    }
                }
            }
            for (dirname, path) in dir_entries {
                if txt_ids.contains(&dirname) {
                    continue;
                }
                let jf = path.join(format!("{dirname}.jsonl"));
                if !jf.exists() {
                    continue;
                }
                let iso = mtime_iso(&jf).unwrap_or_default();
                let date = mtime_date(&jf).unwrap_or_default();
                let first = cursor_first_prompt_jsonl(&jf);
                sessions.push(Session {
                    source: "cursor".into(),
                    id: dirname,
                    summary: String::new(),
                    first_prompt: first,
                    created: iso.clone(),
                    modified: iso,
                    date,
                    messages: 0,
                    branch: String::new(),
                    project: pd.file_name().to_string_lossy().to_string(),
                    file: jf.to_string_lossy().to_string(),
                    is_sidechain: false,
                });
            }
        }
    }
    sessions
}

struct CodexMeta {
    id: String,
    created: String,
    cwd: String,
    branch: String,
    is_subagent: bool,
}

// Codex rollout lines are {"timestamp", "type", "payload": {...}}; older CLI
// versions wrote the payload fields at the top level without a wrapper.
fn codex_payload(entry: &Value) -> &Value {
    entry.get("payload").unwrap_or(entry)
}

fn read_codex_meta(path: &Path) -> Option<CodexMeta> {
    let file = fs::File::open(path).ok()?;
    let reader = BufReader::new(file);
    let line = reader.lines().next()?.ok()?;
    let entry: Value = serde_json::from_str(line.trim()).ok()?;
    let payload = codex_payload(&entry);
    let id = payload.get("id").and_then(Value::as_str)?.to_string();
    let created = payload
        .get("timestamp")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let cwd = payload
        .get("cwd")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let branch = payload
        .get("git")
        .and_then(|g| g.get("branch"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let is_subagent = payload
        .get("source")
        .and_then(|s| s.get("subagent"))
        .is_some();
    Some(CodexMeta {
        id,
        created,
        cwd,
        branch,
        is_subagent,
    })
}

// Codex injects environment/permission wrappers as user-role messages.
fn is_codex_noise(text: &str) -> bool {
    let t = text.trim_start();
    t.starts_with("<environment_context>")
        || t.starts_with("<permissions")
        || t.starts_with("<user_instructions>")
}

fn codex_first_prompt(path: &Path) -> String {
    let file = match fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return String::new(),
    };
    let reader = BufReader::new(file);
    for line in reader.lines().take(100) {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        let entry: Value = match serde_json::from_str(line.trim()) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let payload = codex_payload(&entry);
        if payload.get("type").and_then(Value::as_str) != Some("message")
            || payload.get("role").and_then(Value::as_str) != Some("user")
        {
            continue;
        }
        let content = payload
            .get("content")
            .cloned()
            .unwrap_or(Value::String(String::new()));
        let text = extract_text(&content);
        let trimmed = text.trim();
        if !trimmed.is_empty() && !is_codex_noise(trimmed) {
            return trimmed.chars().take(300).collect();
        }
    }
    String::new()
}

pub fn load_codex_sessions() -> Vec<Session> {
    let base = codex_sessions_dir();
    if !base.exists() {
        return Vec::new();
    }
    let mut sessions = Vec::new();
    for path in glob::glob(&format!("{}/**/rollout-*.jsonl", base.display()))
        .into_iter()
        .flatten()
        .flatten()
    {
        let Some(meta) = read_codex_meta(&path) else {
            continue;
        };
        if meta.is_subagent {
            continue;
        }
        let modified = mtime_iso(&path).unwrap_or_default();
        let date = meta
            .created
            .get(..10)
            .map(str::to_string)
            .unwrap_or_else(|| mtime_date(&path).unwrap_or_default());
        let first = codex_first_prompt(&path);
        sessions.push(Session {
            source: "codex".into(),
            id: meta.id,
            summary: String::new(),
            first_prompt: first,
            created: meta.created,
            modified,
            date,
            messages: 0,
            branch: meta.branch,
            project: meta.cwd,
            file: path.to_string_lossy().to_string(),
            is_sidechain: false,
        });
    }
    sessions
}

pub fn load_all_sessions() -> Vec<Session> {
    let mut all = load_claude_sessions();
    all.extend(load_cursor_sessions());
    all.extend(load_codex_sessions());
    all
}

pub fn parse_claude_jsonl(
    filepath: &str,
    extract_meta: bool,
) -> (Vec<Message>, Option<SessionMeta>) {
    let file = match fs::File::open(filepath) {
        Ok(f) => f,
        Err(_) => {
            return (
                Vec::new(),
                if extract_meta {
                    Some(SessionMeta {
                        summary: None,
                        custom_title: None,
                        model: None,
                        total_tokens: 0,
                    })
                } else {
                    None
                },
            );
        }
    };
    let reader = BufReader::new(file);
    let mut messages = Vec::new();
    let mut meta = SessionMeta {
        summary: None,
        custom_title: None,
        model: None,
        total_tokens: 0,
    };
    let mut skip_next_assistant = false;
    let mut user_texts = Vec::new();
    let mut total_chars: usize = 0;
    let max_chars: usize = 4 * 1024 * 1024;

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        let trimmed = line.trim().to_string();
        if trimmed.is_empty() {
            continue;
        }
        let entry: Value = match serde_json::from_str(&trimmed) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let etype = entry.get("type").and_then(Value::as_str).unwrap_or("");

        if etype == "summary" && extract_meta && meta.summary.is_none() {
            meta.summary = entry
                .get("summary")
                .and_then(Value::as_str)
                .map(String::from);
            continue;
        }
        if etype == "custom_title" && extract_meta && meta.custom_title.is_none() {
            let title = entry
                .get("custom_title")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string();
            if !title.is_empty() {
                meta.custom_title = Some(title);
            }
            continue;
        }

        if etype != "user" && etype != "assistant" {
            continue;
        }

        let msg_obj = entry
            .get("message")
            .cloned()
            .unwrap_or(Value::Object(Default::default()));
        let content_raw = msg_obj
            .get("content")
            .cloned()
            .unwrap_or(Value::String(String::new()));
        let text = extract_text(&content_raw);

        if etype == "user" {
            user_texts.push(text.clone());
            if is_warmup_message(&text) {
                skip_next_assistant = true;
                continue;
            }
            if is_clear_metadata(&text) {
                continue;
            }
        }

        if etype == "assistant" {
            if extract_meta
                && meta.model.is_none()
                && let Some(m) = msg_obj.get("model").and_then(Value::as_str)
            {
                meta.model = Some(m.to_string());
            }
            if extract_meta && let Some(usage) = msg_obj.get("usage") {
                let tok = usage
                    .get("input_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0)
                    + usage
                        .get("output_tokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(0)
                    + usage
                        .get("cache_creation_input_tokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(0)
                    + usage
                        .get("cache_read_input_tokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                meta.total_tokens += tok;
            }
            if skip_next_assistant {
                skip_next_assistant = false;
                continue;
            }
        }

        let ctx = crate::parser::extract_context(&content_raw);

        if total_chars < max_chars {
            let role = msg_obj
                .get("role")
                .and_then(Value::as_str)
                .unwrap_or(etype)
                .to_string();
            let uuid = entry
                .get("uuid")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let timestamp = entry
                .get("timestamp")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let session_id = entry
                .get("sessionId")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let cwd = entry
                .get("cwd")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            total_chars += text.len();
            messages.push(Message {
                uuid,
                timestamp,
                role,
                content: text,
                session_id,
                project_path: cwd,
                tool_uses: ctx.tools,
                files_referenced: ctx.files,
                error_patterns: ctx.errors,
                relevance_score: 0.0,
                final_score: 0.0,
            });
        }
    }

    if extract_meta {
        if crate::parser::is_clear_only_conversation(&user_texts) {
            return (Vec::new(), Some(meta));
        }
        return (messages, Some(meta));
    }
    (messages, None)
}

pub fn parse_codex_jsonl(filepath: &str) -> Vec<Message> {
    let file = match fs::File::open(filepath) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };
    let reader = BufReader::new(file);
    let mut messages: Vec<Message> = Vec::new();
    let mut session_id = String::new();
    let mut project_path = String::new();
    let mut total_chars: usize = 0;
    let max_chars: usize = 4 * 1024 * 1024;

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let entry: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if entry.get("type").and_then(Value::as_str) == Some("session_meta") {
            if let Some(p) = entry.get("payload") {
                session_id = p
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                project_path = p
                    .get("cwd")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
            }
            continue;
        }
        let timestamp = entry
            .get("timestamp")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let payload = codex_payload(&entry);
        match payload.get("type").and_then(Value::as_str) {
            Some("message") => {
                let role = payload.get("role").and_then(Value::as_str).unwrap_or("");
                if role != "user" && role != "assistant" {
                    continue;
                }
                let content_raw = payload
                    .get("content")
                    .cloned()
                    .unwrap_or(Value::String(String::new()));
                let text = extract_text(&content_raw);
                if text.trim().is_empty() || (role == "user" && is_codex_noise(&text)) {
                    continue;
                }
                let ctx = crate::parser::extract_context(&content_raw);
                if total_chars < max_chars {
                    total_chars += text.len();
                    messages.push(Message {
                        uuid: String::new(),
                        timestamp,
                        role: role.to_string(),
                        content: text,
                        session_id: session_id.clone(),
                        project_path: project_path.clone(),
                        tool_uses: ctx.tools,
                        files_referenced: ctx.files,
                        error_patterns: ctx.errors,
                        relevance_score: 0.0,
                        final_score: 0.0,
                    });
                }
            }
            // Tool calls are separate records in Codex rollouts; attach the
            // tool name to the assistant message that initiated it.
            Some("function_call") => {
                if let Some(name) = payload.get("name").and_then(Value::as_str)
                    && let Some(last) = messages.last_mut()
                    && last.role == "assistant"
                {
                    last.tool_uses.push(name.to_string());
                }
            }
            _ => {}
        }
    }
    messages
}

pub fn parse_cursor_jsonl(filepath: &str) -> Vec<Message> {
    let file = match fs::File::open(filepath) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };
    let reader = BufReader::new(file);
    let mut messages = Vec::new();
    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        let trimmed = line.trim().to_string();
        if trimmed.is_empty() {
            continue;
        }
        let entry: Value = match serde_json::from_str(&trimmed) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let role = entry
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let msg_obj = entry
            .get("message")
            .cloned()
            .unwrap_or(Value::Object(Default::default()));
        let content_raw = msg_obj
            .get("content")
            .cloned()
            .unwrap_or(Value::String(String::new()));
        let text = extract_text(&content_raw);
        let ctx = crate::parser::extract_context(&content_raw);
        if !role.is_empty() && !text.trim().is_empty() {
            messages.push(Message {
                uuid: String::new(),
                timestamp: String::new(),
                role,
                content: text,
                session_id: String::new(),
                project_path: String::new(),
                tool_uses: ctx.tools,
                files_referenced: ctx.files,
                error_patterns: ctx.errors,
                relevance_score: 0.0,
                final_score: 0.0,
            });
        }
    }
    messages
}

pub fn parse_cursor_txt(filepath: &str) -> Vec<Message> {
    let content = match fs::read_to_string(filepath) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let mut messages = Vec::new();
    let mut current_role: Option<String> = None;
    let mut current_lines = Vec::new();

    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("user:") {
            if let Some(role) = current_role.take() {
                let text = current_lines.join("\n").trim().to_string();
                if !text.is_empty() {
                    messages.push(Message {
                        uuid: String::new(),
                        timestamp: String::new(),
                        role,
                        content: text,
                        session_id: String::new(),
                        project_path: String::new(),
                        tool_uses: Vec::new(),
                        files_referenced: Vec::new(),
                        error_patterns: Vec::new(),
                        relevance_score: 0.0,
                        final_score: 0.0,
                    });
                }
            }
            current_role = Some("user".into());
            current_lines = vec![rest.trim().to_string()];
        } else if let Some(rest) = line.strip_prefix("assistant:") {
            if let Some(role) = current_role.take() {
                let text = current_lines.join("\n").trim().to_string();
                if !text.is_empty() {
                    messages.push(Message {
                        uuid: String::new(),
                        timestamp: String::new(),
                        role,
                        content: text,
                        session_id: String::new(),
                        project_path: String::new(),
                        tool_uses: Vec::new(),
                        files_referenced: Vec::new(),
                        error_patterns: Vec::new(),
                        relevance_score: 0.0,
                        final_score: 0.0,
                    });
                }
            }
            current_role = Some("assistant".into());
            current_lines = vec![rest.trim().to_string()];
        } else {
            current_lines.push(line.to_string());
        }
    }
    if let Some(role) = current_role {
        let text = current_lines.join("\n").trim().to_string();
        if !text.is_empty() {
            messages.push(Message {
                uuid: String::new(),
                timestamp: String::new(),
                role,
                content: text,
                session_id: String::new(),
                project_path: String::new(),
                tool_uses: Vec::new(),
                files_referenced: Vec::new(),
                error_patterns: Vec::new(),
                relevance_score: 0.0,
                final_score: 0.0,
            });
        }
    }
    messages
}

pub fn parse_session(session: &Session, extract_meta: bool) -> (Vec<Message>, Option<SessionMeta>) {
    if session.source == "claude" {
        return parse_claude_jsonl(&session.file, extract_meta);
    }
    if session.source == "codex" {
        let messages = parse_codex_jsonl(&session.file);
        if extract_meta {
            return (
                messages,
                Some(SessionMeta {
                    summary: None,
                    custom_title: None,
                    model: None,
                    total_tokens: 0,
                }),
            );
        }
        return (messages, None);
    }
    let messages = if session.file.ends_with(".jsonl") {
        parse_cursor_jsonl(&session.file)
    } else {
        parse_cursor_txt(&session.file)
    };
    if extract_meta {
        (
            messages,
            Some(SessionMeta {
                summary: if session.summary.is_empty() {
                    None
                } else {
                    Some(session.summary.clone())
                },
                custom_title: None,
                model: None,
                total_tokens: 0,
            }),
        )
    } else {
        (messages, None)
    }
}

pub fn filter_sessions(
    sessions: &[Session],
    from_date: Option<NaiveDate>,
    to_date: Option<NaiveDate>,
    keyword: Option<&str>,
    source: Option<&str>,
    project: Option<&str>,
    branch: Option<&str>,
) -> Vec<Session> {
    let project_norm = project.map(normalize_for_project_match);
    let mut out: Vec<Session> = sessions
        .iter()
        .filter(|s| {
            if s.date.is_empty() {
                return false;
            }
            let d = match NaiveDate::parse_from_str(&s.date, "%Y-%m-%d") {
                Ok(d) => d,
                Err(_) => return false,
            };
            if let Some(fd) = from_date
                && d < fd
            {
                return false;
            }
            if let Some(td) = to_date
                && d > td
            {
                return false;
            }
            if let Some(src) = source
                && s.source != src
            {
                return false;
            }
            if let Some(proj_norm) = project_norm.as_ref()
                && !normalize_for_project_match(&s.project).contains(proj_norm)
            {
                return false;
            }
            if let Some(br) = branch
                && !s.branch.to_lowercase().contains(&br.to_lowercase())
            {
                return false;
            }
            if let Some(kw) = keyword {
                let kw_lower = kw.to_lowercase();
                let haystack = format!(
                    "{} {} {} {}",
                    s.summary, s.first_prompt, s.branch, s.project
                )
                .to_lowercase();
                if !haystack.contains(&kw_lower) {
                    return false;
                }
            }
            true
        })
        .cloned()
        .collect();
    out.sort_by(|a, b| {
        let ma = if a.modified.is_empty() {
            &a.created
        } else {
            &a.modified
        };
        let mb = if b.modified.is_empty() {
            &b.created
        } else {
            &b.modified
        };
        mb.cmp(ma)
    });
    out
}

pub fn find_session<'a>(sessions: &'a [Session], sid: &str) -> Option<&'a Session> {
    sessions
        .iter()
        .find(|s| s.id == sid)
        .or_else(|| sessions.iter().find(|s| s.id.starts_with(sid)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_session(
        id: &str,
        date: &str,
        source: &str,
        project: &str,
        branch: &str,
        summary: &str,
    ) -> Session {
        Session {
            source: source.into(),
            id: id.into(),
            summary: summary.into(),
            first_prompt: String::new(),
            created: format!("{date}T00:00:00"),
            modified: String::new(),
            date: date.into(),
            messages: 0,
            branch: branch.into(),
            project: project.into(),
            file: String::new(),
            is_sidechain: false,
        }
    }

    #[test]
    fn encode_path_basic() {
        let path = std::path::Path::new("/Users/ayush/Documents/project");
        assert_eq!(
            encode_path_for_claude(path),
            "-Users-ayush-Documents-project"
        );
    }

    #[test]
    fn encode_path_root() {
        assert_eq!(encode_path_for_claude(std::path::Path::new("/")), "-");
    }

    #[test]
    fn find_session_exact_match() {
        let sessions = vec![make_session(
            "abc-123",
            "2025-01-01",
            "claude",
            "/proj",
            "main",
            "test",
        )];
        assert!(find_session(&sessions, "abc-123").is_some());
    }

    #[test]
    fn find_session_prefix_match() {
        let sessions = vec![make_session(
            "abc-123-def-456",
            "2025-01-01",
            "claude",
            "/proj",
            "main",
            "test",
        )];
        let found = find_session(&sessions, "abc-123");
        assert!(found.is_some());
        assert_eq!(found.unwrap().id, "abc-123-def-456");
    }

    #[test]
    fn find_session_not_found() {
        let sessions = vec![make_session(
            "abc-123",
            "2025-01-01",
            "claude",
            "/proj",
            "main",
            "test",
        )];
        assert!(find_session(&sessions, "xyz-999").is_none());
    }

    #[test]
    fn filter_by_source() {
        let sessions = vec![
            make_session("1", "2025-01-01", "claude", "/proj", "", "s1"),
            make_session("2", "2025-01-01", "cursor", "/proj", "", "s2"),
        ];
        let filtered = filter_sessions(&sessions, None, None, None, Some("claude"), None, None);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].source, "claude");
    }

    #[test]
    fn filter_by_project() {
        let sessions = vec![
            make_session("1", "2025-01-01", "claude", "chat-history", "", "s1"),
            make_session("2", "2025-01-01", "claude", "other-project", "", "s2"),
        ];
        let filtered = filter_sessions(&sessions, None, None, None, None, Some("chat"), None);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].project, "chat-history");
    }

    #[test]
    fn filter_by_project_underscore_dash_mismatch() {
        let sessions = vec![make_session(
            "1",
            "2025-01-01",
            "cursor",
            "proj-one-two-123",
            "",
            "",
        )];
        let filtered = filter_sessions(
            &sessions,
            None,
            None,
            None,
            None,
            Some("proj_one_two_123"),
            None,
        );
        assert_eq!(
            filtered.len(),
            1,
            "underscore in filter should match dashes in project path"
        );
    }

    #[test]
    fn filter_by_branch() {
        let sessions = vec![
            make_session("1", "2025-01-01", "claude", "/proj", "main", "s1"),
            make_session("2", "2025-01-01", "claude", "/proj", "feature-x", "s2"),
        ];
        let filtered = filter_sessions(&sessions, None, None, None, None, None, Some("feature"));
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].branch, "feature-x");
    }

    #[test]
    fn filter_by_keyword() {
        let sessions = vec![
            make_session("1", "2025-01-01", "claude", "/proj", "", "implement auth"),
            make_session("2", "2025-01-01", "claude", "/proj", "", "fix docker build"),
        ];
        let filtered = filter_sessions(&sessions, None, None, Some("docker"), None, None, None);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].summary, "fix docker build");
    }

    #[test]
    fn filter_by_date_range() {
        let sessions = vec![
            make_session("1", "2025-01-01", "claude", "/proj", "", "old"),
            make_session("2", "2025-06-15", "claude", "/proj", "", "new"),
        ];
        let from = NaiveDate::from_ymd_opt(2025, 6, 1).unwrap();
        let filtered = filter_sessions(&sessions, Some(from), None, None, None, None, None);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].summary, "new");
    }

    #[test]
    fn filter_by_to_date() {
        let sessions = vec![
            make_session("1", "2025-01-01", "claude", "/proj", "", "old"),
            make_session("2", "2025-06-15", "claude", "/proj", "", "new"),
        ];
        let to = NaiveDate::from_ymd_opt(2025, 3, 1).unwrap();
        let filtered = filter_sessions(&sessions, None, Some(to), None, None, None, None);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].summary, "old");
    }

    #[test]
    fn filter_excludes_empty_date() {
        let sessions = vec![make_session("1", "", "claude", "/proj", "", "no date")];
        let filtered = filter_sessions(&sessions, None, None, None, None, None, None);
        assert!(filtered.is_empty());
    }

    #[test]
    fn filter_sorted_by_recency() {
        let sessions = vec![
            make_session("1", "2025-01-01", "claude", "/proj", "", "old"),
            make_session("2", "2025-06-15", "claude", "/proj", "", "new"),
            make_session("3", "2025-03-10", "claude", "/proj", "", "mid"),
        ];
        let filtered = filter_sessions(&sessions, None, None, None, None, None, None);
        assert_eq!(filtered[0].summary, "new");
        assert_eq!(filtered[1].summary, "mid");
        assert_eq!(filtered[2].summary, "old");
    }

    #[test]
    fn filter_keyword_case_insensitive() {
        let sessions = vec![make_session(
            "1",
            "2025-01-01",
            "claude",
            "/proj",
            "",
            "Docker Build",
        )];
        let filtered = filter_sessions(&sessions, None, None, Some("docker"), None, None, None);
        assert_eq!(filtered.len(), 1);
    }

    #[test]
    fn filter_combined() {
        let sessions = vec![
            make_session(
                "1",
                "2025-06-15",
                "claude",
                "chat-history",
                "main",
                "fix auth",
            ),
            make_session(
                "2",
                "2025-06-15",
                "cursor",
                "chat-history",
                "main",
                "fix docker",
            ),
            make_session(
                "3",
                "2025-06-15",
                "claude",
                "other-proj",
                "main",
                "fix auth",
            ),
        ];
        let filtered = filter_sessions(
            &sessions,
            None,
            None,
            None,
            Some("claude"),
            Some("chat"),
            None,
        );
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].id, "1");
    }

    #[test]
    fn parse_claude_jsonl_with_fixture() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let data = r#"{"type":"user","message":{"role":"user","content":"hello world"},"timestamp":"2025-01-01T00:00:00Z","uuid":"u1"}
{"type":"assistant","message":{"role":"assistant","content":"Hi there! How can I help you today?"},"timestamp":"2025-01-01T00:01:00Z","uuid":"u2"}"#;
        std::fs::write(tmp.path(), data).unwrap();
        let (messages, _) = parse_claude_jsonl(tmp.path().to_str().unwrap(), false);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, "user");
        assert_eq!(messages[0].content, "hello world");
        assert_eq!(messages[1].role, "assistant");
    }

    #[test]
    fn parse_claude_jsonl_skips_warmup() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let data = r#"{"type":"user","message":{"role":"user","content":"warmup"},"timestamp":"2025-01-01T00:00:00Z","uuid":"u1"}
{"type":"assistant","message":{"role":"assistant","content":"warmed up"},"timestamp":"2025-01-01T00:01:00Z","uuid":"u2"}
{"type":"user","message":{"role":"user","content":"real question here"},"timestamp":"2025-01-01T00:02:00Z","uuid":"u3"}"#;
        std::fs::write(tmp.path(), data).unwrap();
        let (messages, _) = parse_claude_jsonl(tmp.path().to_str().unwrap(), false);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content, "real question here");
    }

    #[test]
    fn parse_claude_jsonl_extracts_meta() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let data = r#"{"type":"summary","summary":"Test session summary"}
{"type":"user","message":{"role":"user","content":"test prompt"},"timestamp":"2025-01-01T00:00:00Z","uuid":"u1"}
{"type":"assistant","message":{"role":"assistant","content":"response","model":"claude-3-opus","usage":{"input_tokens":100,"output_tokens":50}},"timestamp":"2025-01-01T00:01:00Z","uuid":"u2"}"#;
        std::fs::write(tmp.path(), data).unwrap();
        let (messages, meta) = parse_claude_jsonl(tmp.path().to_str().unwrap(), true);
        assert_eq!(messages.len(), 2);
        let meta = meta.unwrap();
        assert_eq!(meta.summary.as_deref(), Some("Test session summary"));
        assert_eq!(meta.model.as_deref(), Some("claude-3-opus"));
        assert_eq!(meta.total_tokens, 150);
    }

    #[test]
    fn parse_claude_jsonl_nonexistent_file() {
        let (messages, _) = parse_claude_jsonl("/nonexistent/path.jsonl", false);
        assert!(messages.is_empty());
    }

    #[test]
    fn parse_cursor_txt_basic() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let data = "title line\nuser: hello world\nassistant: hi there\nuser: another question\nassistant: another answer";
        std::fs::write(tmp.path(), data).unwrap();
        let messages = parse_cursor_txt(tmp.path().to_str().unwrap());
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[0].role, "user");
        assert!(messages[0].content.contains("hello world"));
        assert_eq!(messages[1].role, "assistant");
        assert_eq!(messages[2].role, "user");
        assert_eq!(messages[3].role, "assistant");
    }

    const CODEX_FIXTURE: &str = r#"{"timestamp":"2026-06-10T10:00:00.000Z","type":"session_meta","payload":{"id":"019e0000-aaaa-bbbb-cccc-000000000001","timestamp":"2026-06-10T10:00:00.000Z","cwd":"/Users/test/myproj","originator":"codex-tui","cli_version":"0.136.0","git":{"commit_hash":"abc123","branch":"feature-codex"}}}
{"timestamp":"2026-06-10T10:00:01.000Z","type":"response_item","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"<permissions instructions>\nsandbox stuff"}]}}
{"timestamp":"2026-06-10T10:00:01.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context>\n<cwd>/Users/test/myproj</cwd>\n</environment_context>"}]}}
{"timestamp":"2026-06-10T10:00:02.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"add connection pooling to the database layer"}]}}
{"timestamp":"2026-06-10T10:00:05.000Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"I'll add connection pooling using a bounded pool."}],"phase":"commentary"}}
{"timestamp":"2026-06-10T10:00:06.000Z","type":"response_item","payload":{"type":"function_call","name":"shell","arguments":"{\"command\":[\"ls\"]}","call_id":"call_1"}}
{"timestamp":"2026-06-10T10:00:07.000Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call_1","output":"src tests"}}
{"timestamp":"2026-06-10T10:00:09.000Z","type":"event_msg","payload":{"type":"token_count","info":{}}}"#;

    #[test]
    fn parse_codex_jsonl_with_fixture() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), CODEX_FIXTURE).unwrap();
        let messages = parse_codex_jsonl(tmp.path().to_str().unwrap());
        assert_eq!(
            messages.len(),
            2,
            "developer and environment_context messages should be filtered"
        );
        assert_eq!(messages[0].role, "user");
        assert_eq!(
            messages[0].content,
            "add connection pooling to the database layer"
        );
        assert_eq!(
            messages[0].session_id,
            "019e0000-aaaa-bbbb-cccc-000000000001"
        );
        assert_eq!(messages[0].project_path, "/Users/test/myproj");
        assert_eq!(messages[0].timestamp, "2026-06-10T10:00:02.000Z");
        assert_eq!(messages[1].role, "assistant");
        assert_eq!(
            messages[1].tool_uses,
            vec!["shell".to_string()],
            "function_call tool name should attach to preceding assistant message"
        );
    }

    #[test]
    fn parse_codex_jsonl_legacy_flat_records() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let data = r#"{"id":"legacy-id","timestamp":"2025-08-01T10:00:00.000Z","instructions":null}
{"type":"message","role":"user","content":[{"type":"input_text","text":"legacy format question"}]}
{"type":"message","role":"assistant","content":[{"type":"output_text","text":"legacy format answer"}]}"#;
        std::fs::write(tmp.path(), data).unwrap();
        let messages = parse_codex_jsonl(tmp.path().to_str().unwrap());
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].content, "legacy format question");
        assert_eq!(messages[1].content, "legacy format answer");
    }

    #[test]
    fn parse_codex_jsonl_nonexistent_file() {
        assert!(parse_codex_jsonl("/nonexistent/rollout.jsonl").is_empty());
    }

    #[test]
    fn codex_first_prompt_skips_noise() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), CODEX_FIXTURE).unwrap();
        assert_eq!(
            codex_first_prompt(tmp.path()),
            "add connection pooling to the database layer"
        );
    }

    #[test]
    fn read_codex_meta_basic() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), CODEX_FIXTURE).unwrap();
        let meta = read_codex_meta(tmp.path()).unwrap();
        assert_eq!(meta.id, "019e0000-aaaa-bbbb-cccc-000000000001");
        assert_eq!(meta.created, "2026-06-10T10:00:00.000Z");
        assert_eq!(meta.cwd, "/Users/test/myproj");
        assert_eq!(meta.branch, "feature-codex");
        assert!(!meta.is_subagent);
    }

    #[test]
    fn read_codex_meta_detects_subagent() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let data = r#"{"timestamp":"2026-06-10T10:00:00.000Z","type":"session_meta","payload":{"id":"sub-1","timestamp":"2026-06-10T10:00:00.000Z","cwd":"/p","source":{"subagent":{"parent_thread_id":"parent-1","depth":1}}}}"#;
        std::fs::write(tmp.path(), data).unwrap();
        assert!(read_codex_meta(tmp.path()).unwrap().is_subagent);
    }

    #[test]
    fn copy_session_to_dir_creates_target() {
        let tmp = tempfile::TempDir::new().unwrap();
        let src_file = tmp.path().join("test.jsonl");
        std::fs::write(&src_file, "test data").unwrap();

        let session = Session {
            source: "claude".into(),
            id: "test-id".into(),
            summary: String::new(),
            first_prompt: String::new(),
            created: String::new(),
            modified: String::new(),
            date: String::new(),
            messages: 0,
            branch: String::new(),
            project: String::new(),
            file: src_file.to_string_lossy().to_string(),
            is_sidechain: false,
        };

        let target = tmp.path().join("target-dir");
        copy_session_to_dir(&session, &target).unwrap();
        assert!(target.join("test.jsonl").exists());
        assert_eq!(
            std::fs::read_to_string(target.join("test.jsonl")).unwrap(),
            "test data"
        );
    }

    #[test]
    fn read_cwd_from_jsonl_extracts_cwd() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let data = r#"{"type":"user","cwd":"/Users/test/project","message":{"content":"hello"}}"#;
        std::fs::write(tmp.path(), data).unwrap();
        assert_eq!(
            read_cwd_from_jsonl(tmp.path()),
            Some("/Users/test/project".to_string())
        );
    }

    #[test]
    fn read_cwd_from_jsonl_no_cwd() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let data = r#"{"type":"user","message":{"content":"hello"}}"#;
        std::fs::write(tmp.path(), data).unwrap();
        assert_eq!(read_cwd_from_jsonl(tmp.path()), None);
    }

    #[test]
    fn parse_any_timestamp_rfc3339() {
        assert!(parse_any_timestamp("2025-01-15T10:30:00+00:00").is_some());
    }

    #[test]
    fn parse_any_timestamp_z_suffix() {
        assert!(parse_any_timestamp("2025-01-15T10:30:00Z").is_some());
    }

    #[test]
    fn parse_any_timestamp_invalid() {
        assert!(parse_any_timestamp("not-a-date").is_none());
        assert!(parse_any_timestamp("").is_none());
    }

    #[test]
    fn mtime_iso_appends_z() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let ts = mtime_iso(tmp.path()).unwrap();
        assert!(
            ts.ends_with('Z'),
            "mtime_iso should produce Z-suffixed timestamps, got: {ts}"
        );
        assert!(
            parse_any_timestamp(&ts).is_some(),
            "mtime_iso output should be parseable"
        );
    }
}

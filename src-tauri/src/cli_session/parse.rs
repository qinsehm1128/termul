//! Metadata-only parsers. First cwd wins. Session ids come from transcript
//! fields, not file names, except where the vendor format requires it.

use std::fs;
use std::path::Path;
use std::time::SystemTime;

use serde_json::Value;

use super::types::{
    first_cwd_wins, normalize_session_id, session_list_id, CliSessionAgentId, DiscoveredCliSession,
    SCHEMA_VERSION,
};

#[derive(Default)]
struct Accumulator {
    session_id: Option<String>,
    cwd: Option<String>,
    title: Option<String>,
    created_at: Option<String>,
    updated_at: Option<String>,
    message_count: u32,
    has_user: bool,
    has_assistant: bool,
}

pub fn parse_session_file(
    agent: CliSessionAgentId,
    path: &Path,
    codex_home: Option<&Path>,
) -> Option<DiscoveredCliSession> {
    match agent {
        CliSessionAgentId::ClaudeCode => parse_claude(path),
        CliSessionAgentId::Codex => parse_codex(path, codex_home),
        CliSessionAgentId::GeminiCli => parse_gemini(path),
        CliSessionAgentId::Cursor => parse_cursor(path),
        CliSessionAgentId::Opencode => parse_opencode(path),
        CliSessionAgentId::Pi => parse_pi(path),
    }
}

fn parse_claude(path: &Path) -> Option<DiscoveredCliSession> {
    let mut acc = Accumulator::default();
    for value in jsonl_values(path) {
        if acc.session_id.is_none() {
            acc.session_id = string_field(&value, &["sessionId", "session_id"])
                .and_then(|id| normalize_session_id(&id));
        }
        first_cwd_wins(&mut acc.cwd, string_field(&value, &["cwd"]));
        note_turn(&mut acc, &value);
        if acc.title.is_none() {
            acc.title = first_user_text(&value);
        }
        touch_timestamps(&mut acc, &value);
    }
    finalize(CliSessionAgentId::ClaudeCode, path, acc, None, None)
}

fn parse_codex(path: &Path, codex_home: Option<&Path>) -> Option<DiscoveredCliSession> {
    let mut acc = Accumulator::default();
    for value in jsonl_values(path) {
        let payload = value.get("payload").cloned().unwrap_or(value.clone());
        if acc.session_id.is_none() {
            acc.session_id = string_field(&payload, &["id", "session_id", "sessionId"])
                .and_then(|id| normalize_session_id(&id));
        }
        first_cwd_wins(
            &mut acc.cwd,
            string_field(&payload, &["cwd"]).or_else(|| {
                payload
                    .get("cwd")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned)
            }),
        );
        note_turn(&mut acc, &payload);
        if acc.title.is_none() {
            acc.title = first_user_text(&payload);
        }
        touch_timestamps(&mut acc, &payload);
        touch_timestamps(&mut acc, &value);
    }
    if acc.session_id.is_none() {
        acc.session_id = session_id_from_codex_filename(path);
    }
    let home = codex_home.map(|p| p.display().to_string());
    finalize(CliSessionAgentId::Codex, path, acc, home, None)
}

fn parse_gemini(path: &Path) -> Option<DiscoveredCliSession> {
    let mut acc = Accumulator::default();
    if looks_like_jsonl(path) {
        for value in jsonl_values(path) {
            absorb_gemini(&mut acc, &value);
        }
    } else if let Some(value) = read_json(path) {
        absorb_gemini(&mut acc, &value);
        if let Some(messages) = value.get("messages").and_then(Value::as_array) {
            for message in messages {
                note_turn(&mut acc, message);
                if acc.title.is_none() {
                    acc.title = first_user_text(message);
                }
            }
        }
    }
    finalize(CliSessionAgentId::GeminiCli, path, acc, None, None)
}

fn absorb_gemini(acc: &mut Accumulator, value: &Value) {
    if acc.session_id.is_none() {
        acc.session_id = string_field(value, &["sessionId", "session_id", "id"])
            .and_then(|id| normalize_session_id(&id));
    }
    first_cwd_wins(
        &mut acc.cwd,
        string_field(value, &["cwd"]).or_else(|| {
            value
                .pointer("/project/root")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        }),
    );
    note_turn(acc, value);
    if acc.title.is_none() {
        acc.title = first_user_text(value);
    }
    touch_timestamps(acc, value);
}

fn parse_cursor(path: &Path) -> Option<DiscoveredCliSession> {
    let mut acc = Accumulator::default();
    for value in jsonl_values(path) {
        if acc.session_id.is_none() {
            acc.session_id = string_field(&value, &["sessionId", "session_id", "id"])
                .and_then(|id| normalize_session_id(&id));
        }
        first_cwd_wins(&mut acc.cwd, string_field(&value, &["cwd"]));
        note_turn(&mut acc, &value);
        if acc.title.is_none() {
            acc.title = first_user_text(&value);
        }
        touch_timestamps(&mut acc, &value);
    }
    if acc.session_id.is_none() {
        acc.session_id = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .and_then(normalize_session_id);
    }
    finalize(CliSessionAgentId::Cursor, path, acc, None, None)
}

fn parse_opencode(path: &Path) -> Option<DiscoveredCliSession> {
    let value = read_json(path)?;
    let mut acc = Accumulator::default();
    acc.session_id = string_field(&value, &["id", "sessionID", "sessionId"])
        .and_then(|id| normalize_session_id(&id));
    first_cwd_wins(
        &mut acc.cwd,
        string_field(&value, &["directory", "cwd", "project"]),
    );
    acc.title = string_field(&value, &["title", "name"]);
    if let Some(time) = value.get("time") {
        acc.created_at = timestamp_from_value(time.get("created"));
        acc.updated_at = timestamp_from_value(time.get("updated"));
    }
    touch_timestamps(&mut acc, &value);
    if let Some(messages) = value
        .get("messages")
        .or_else(|| value.get("parts"))
        .and_then(Value::as_array)
    {
        for message in messages {
            note_turn(&mut acc, message);
            if acc.title.is_none() {
                acc.title = first_user_text(message);
            }
        }
    }
    if acc.message_count == 0 && acc.title.is_some() {
        acc.has_user = true;
        acc.message_count = 1;
    }
    finalize(CliSessionAgentId::Opencode, path, acc, None, None)
}

fn parse_pi(path: &Path) -> Option<DiscoveredCliSession> {
    let mut acc = Accumulator::default();
    for value in jsonl_values(path) {
        let kind = value.get("type").and_then(Value::as_str).unwrap_or("");
        if kind == "session" && acc.session_id.is_none() {
            acc.session_id =
                string_field(&value, &["id", "session_id"]).and_then(|id| normalize_session_id(&id));
        }
        if acc.session_id.is_none() {
            acc.session_id = string_field(&value, &["session_id", "sessionId", "id"])
                .and_then(|id| normalize_session_id(&id));
        }
        first_cwd_wins(&mut acc.cwd, string_field(&value, &["cwd"]));
        note_turn(&mut acc, &value);
        if acc.title.is_none() {
            acc.title = first_user_text(&value);
        }
        touch_timestamps(&mut acc, &value);
    }
    let resume_path = path.to_string_lossy().to_string();
    finalize(
        CliSessionAgentId::Pi,
        path,
        acc,
        None,
        Some(resume_path),
    )
}

fn finalize(
    agent: CliSessionAgentId,
    path: &Path,
    acc: Accumulator,
    codex_home: Option<String>,
    resume_file_path: Option<String>,
) -> Option<DiscoveredCliSession> {
    let session_id = acc.session_id?;
    let file_path = path.to_string_lossy().to_string();
    let resumable = acc.has_user || acc.has_assistant || acc.message_count > 0;
    let title = acc
        .title
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| format!("{} session", agent.as_str()));
    let mtime = file_mtime_rfc3339(path);
    Some(DiscoveredCliSession {
        schema_version: SCHEMA_VERSION,
        id: session_list_id(agent, &session_id, &file_path),
        agent_id: agent,
        session_id,
        cwd: acc.cwd,
        title,
        created_at: acc.created_at.or_else(|| mtime.clone()),
        updated_at: acc.updated_at.or(mtime),
        message_count: acc.message_count,
        file_path,
        codex_home,
        resume_file_path,
        resumable,
    })
}

fn note_turn(acc: &mut Accumulator, value: &Value) {
    let role = value
        .get("role")
        .and_then(Value::as_str)
        .or_else(|| value.get("type").and_then(Value::as_str))
        .unwrap_or("");
    let kind = role.to_ascii_lowercase();
    if kind.contains("user") {
        acc.has_user = true;
        acc.message_count = acc.message_count.saturating_add(1);
    } else if kind.contains("assistant") || kind == "response" {
        acc.has_assistant = true;
        acc.message_count = acc.message_count.saturating_add(1);
    }
}

fn first_user_text(value: &Value) -> Option<String> {
    let role = value
        .get("role")
        .and_then(Value::as_str)
        .or_else(|| value.get("type").and_then(Value::as_str))
        .unwrap_or("");
    if !role.to_ascii_lowercase().contains("user") {
        if let Some(summary) = string_field(value, &["summary", "title"]) {
            return Some(truncate(&summary, 120));
        }
        return None;
    }
    if let Some(text) = extract_text(value.get("message").unwrap_or(value)) {
        return Some(truncate(&text, 120));
    }
    extract_text(value).map(|text| truncate(&text, 120))
}

fn extract_text(value: &Value) -> Option<String> {
    if let Some(text) = value.get("content").and_then(Value::as_str) {
        return nonempty(text);
    }
    if let Some(text) = value.get("text").and_then(Value::as_str) {
        return nonempty(text);
    }
    if let Some(items) = value.get("content").and_then(Value::as_array) {
        for item in items {
            if let Some(text) = item.get("text").and_then(Value::as_str) {
                if let Some(out) = nonempty(text) {
                    return Some(out);
                }
            }
        }
    }
    value
        .pointer("/message/content")
        .and_then(|content| match content {
            Value::String(text) => nonempty(text),
            Value::Array(items) => items
                .iter()
                .filter_map(|item| item.get("text").and_then(Value::as_str).and_then(nonempty))
                .next(),
            _ => None,
        })
}

fn touch_timestamps(acc: &mut Accumulator, value: &Value) {
    let stamp = string_field(value, &["timestamp", "createdAt", "created_at", "time"])
        .or_else(|| timestamp_from_value(value.get("timestamp")));
    if acc.created_at.is_none() {
        acc.created_at = stamp.clone();
    }
    if let Some(stamp) = stamp {
        acc.updated_at = Some(stamp);
    }
}

fn timestamp_from_value(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(text) if !text.is_empty() => Some(text.clone()),
        Value::Number(number) => number.as_i64().map(|ms| {
            chrono::DateTime::from_timestamp_millis(ms)
                .map(|dt| dt.to_rfc3339())
                .unwrap_or_else(|| ms.to_string())
        }),
        _ => None,
    }
}

fn string_field(value: &Value, keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Some(text) = value.get(*key).and_then(Value::as_str) {
            if let Some(out) = nonempty(text) {
                return Some(out);
            }
        }
    }
    None
}

fn nonempty(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn truncate(text: &str, max: usize) -> String {
    let mut out = String::new();
    for (index, ch) in text.chars().enumerate() {
        if index >= max {
            out.push('…');
            break;
        }
        out.push(ch);
    }
    out
}

fn looks_like_jsonl(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("jsonl"))
}

fn read_json(path: &Path) -> Option<Value> {
    let bytes = fs::read_to_string(path).ok()?;
    serde_json::from_str(&bytes).ok()
}

fn jsonl_values(path: &Path) -> Vec<Value> {
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                return None;
            }
            serde_json::from_str(trimmed).ok()
        })
        .collect()
}

fn file_mtime_rfc3339(path: &Path) -> Option<String> {
    let meta = fs::metadata(path).ok()?;
    let modified = meta.modified().ok()?;
    let duration = modified.duration_since(SystemTime::UNIX_EPOCH).ok()?;
    chrono::DateTime::from_timestamp(duration.as_secs() as i64, duration.subsec_nanos())
        .map(|dt| dt.to_rfc3339())
}

fn session_id_from_codex_filename(path: &Path) -> Option<String> {
    let name = path.file_stem()?.to_str()?;
    // rollout-<ts>-<uuid>
    name.rsplit_once('-')
        .map(|(_, id)| id.to_string())
        .and_then(|id| normalize_session_id(&id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;

    #[test]
    fn claude_uses_jsonl_session_id_and_first_cwd() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("chat.jsonl");
        let mut file = fs::File::create(&path).unwrap();
        writeln!(
            file,
            r#"{{"type":"user","role":"user","sessionId":"sess-1","cwd":"/first","content":[{{"type":"text","text":"hello"}}]}}"#
        )
        .unwrap();
        writeln!(
            file,
            r#"{{"type":"assistant","role":"assistant","cwd":"/later","content":"ok"}}"#
        )
        .unwrap();
        let session = parse_claude(&path).expect("session");
        assert_eq!(session.session_id, "sess-1");
        assert_eq!(session.cwd.as_deref(), Some("/first"));
        assert!(session.resumable);
        assert!(session.title.contains("hello"));
    }

    #[test]
    fn skips_subagent_style_missing_id() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("empty.jsonl");
        fs::write(&path, "{\"type\":\"system\"}\n").unwrap();
        assert!(parse_claude(&path).is_none());
    }

    #[test]
    fn pi_keeps_resume_file_path() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("pi.jsonl");
        fs::write(
            &path,
            r#"{"type":"session","id":"pi-1","cwd":"/repo"}
{"type":"user","role":"user","content":"hi"}
"#,
        )
        .unwrap();
        let session = parse_pi(&path).expect("pi");
        assert_eq!(session.resume_file_path.as_deref(), Some(path.to_str().unwrap()));
        assert_eq!(session.session_id, "pi-1");
    }
}

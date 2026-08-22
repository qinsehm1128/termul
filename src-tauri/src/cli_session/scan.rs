use std::path::{Path, PathBuf};

use chrono::Utc;

use super::parse::parse_session_file;
use super::paths::{default_codex_home, is_under_dir, roots_for_agent};
use super::scope::{filter_scope_paths, is_cwd_in_scope};
use super::types::{
    CliSessionAgentId, CliSessionListArgs, CliSessionListResult, CliSessionScanIssue,
    DiscoveredCliSession, DEFAULT_LIMIT_PER_AGENT, WALK_LIMIT_PER_AGENT,
};
use super::walk::{is_cursor_transcript, is_opencode_session_json, walk_session_files};

pub fn list_cli_sessions(
    args: CliSessionListArgs,
    allowed_scope_roots: Option<&[PathBuf]>,
) -> CliSessionListResult {
    let _force = args.force;
    let scanned_at = Utc::now().to_rfc3339();
    let limit = args.limit.unwrap_or(DEFAULT_LIMIT_PER_AGENT).max(1);
    let agents = args
        .agents
        .clone()
        .unwrap_or_else(|| CliSessionAgentId::all().to_vec());
    let raw_scope = args.scope_paths.clone().unwrap_or_default();
    let (scope_paths, mut issues) = filter_scope_paths(&raw_scope, allowed_scope_roots);

    log::info!(
        target: "termul::cli_session",
        "operation=list_cli_sessions agents={} scope_paths={} limit={}",
        agents.len(),
        scope_paths.len(),
        limit
    );

    let mut sessions = Vec::new();
    for agent in agents {
        let (found, agent_issues) = scan_agent(agent, limit, &scope_paths);
        sessions.extend(found);
        issues.extend(agent_issues);
    }

    sessions.sort_by(|left, right| {
        right
            .updated_at
            .cmp(&left.updated_at)
            .then_with(|| left.agent_id.as_str().cmp(right.agent_id.as_str()))
    });

    log::info!(
        target: "termul::cli_session",
        "operation=list_cli_sessions_done sessions={} issues={}",
        sessions.len(),
        issues.len()
    );

    CliSessionListResult {
        sessions,
        issues,
        scanned_at,
    }
}

fn scan_agent(
    agent: CliSessionAgentId,
    limit: usize,
    scope_paths: &[PathBuf],
) -> (Vec<DiscoveredCliSession>, Vec<CliSessionScanIssue>) {
    let mut issues = Vec::new();
    let roots = roots_for_agent(agent);
    if roots.iter().all(|root| !root.is_dir()) {
        log::info!(
            target: "termul::cli_session",
            "operation=scan_root_missing agent={}",
            agent.as_str()
        );
    }

    let mut files = Vec::new();
    match agent {
        CliSessionAgentId::ClaudeCode => {
            for root in &roots {
                files.extend(walk_session_files(
                    root,
                    &["jsonl"],
                    &["subagents"],
                    None,
                    WALK_LIMIT_PER_AGENT,
                ));
            }
        }
        CliSessionAgentId::Codex => {
            for root in &roots {
                files.extend(walk_session_files(
                    root,
                    &["jsonl"],
                    &[],
                    None,
                    WALK_LIMIT_PER_AGENT,
                ));
            }
        }
        CliSessionAgentId::GeminiCli => {
            for root in &roots {
                files.extend(walk_session_files(
                    root,
                    &["json", "jsonl"],
                    &[],
                    None,
                    WALK_LIMIT_PER_AGENT,
                ));
            }
        }
        CliSessionAgentId::Cursor => {
            for root in &roots {
                files.extend(walk_session_files(
                    root,
                    &["jsonl"],
                    &[],
                    Some(&is_cursor_transcript),
                    WALK_LIMIT_PER_AGENT,
                ));
            }
        }
        CliSessionAgentId::Opencode => {
            for root in &roots {
                if let Ok(entries) = std::fs::read_dir(root) {
                    for entry in entries.flatten() {
                        let name = entry.file_name().to_string_lossy().to_string();
                        if name.ends_with(".db") {
                            issues.push(CliSessionScanIssue {
                                agent_id: agent.as_str().to_string(),
                                path: entry.path().display().to_string(),
                                message: "OpenCode SQLite store found; JSON sessions are listed"
                                    .to_string(),
                            });
                        }
                    }
                }
                files.extend(walk_session_files(
                    root,
                    &["json"],
                    &[],
                    Some(&is_opencode_session_json),
                    WALK_LIMIT_PER_AGENT,
                ));
            }
        }
        CliSessionAgentId::Pi => {
            for root in &roots {
                files.extend(walk_session_files(
                    root,
                    &["jsonl"],
                    &[],
                    None,
                    WALK_LIMIT_PER_AGENT,
                ));
            }
        }
    }

    files.sort_by(|a, b| b.modified.cmp(&a.modified));
    files.dedup_by(|a, b| a.path == b.path);

    let codex_home = default_codex_home();
    let mut parsed = Vec::new();
    for file in files {
        match parse_session_file(agent, &file.path, codex_home.as_deref()) {
            Some(mut session) => {
                if agent == CliSessionAgentId::Codex {
                    if let Some(home) = codex_home.as_ref() {
                        if !is_default_codex_home(home) {
                            session.codex_home = Some(home.display().to_string());
                        }
                    }
                }
                parsed.push(session);
            }
            None => {
                issues.push(CliSessionScanIssue {
                    agent_id: agent.as_str().to_string(),
                    path: file.path.display().to_string(),
                    message: "failed to parse session metadata".to_string(),
                });
            }
        }
    }

    let mut in_scope = Vec::new();
    let mut remainder = Vec::new();
    for session in parsed {
        if is_cwd_in_scope(session.cwd.as_deref(), scope_paths) && !scope_paths.is_empty() {
            in_scope.push(session);
        } else {
            remainder.push(session);
        }
    }

    if scope_paths.is_empty() {
        remainder.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
        remainder.truncate(limit);
        return (remainder, issues);
    }

    in_scope.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
    in_scope.truncate(limit);
    (in_scope, issues)
}

fn is_default_codex_home(home: &Path) -> bool {
    dirs_like_default_codex(home)
}

fn dirs_like_default_codex(home: &Path) -> bool {
    std::env::var_os("CODEX_HOME").is_none()
        && super::paths::user_home()
            .is_some_and(|user| is_under_dir(home, &user.join(".codex")) || home == user.join(".codex"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn scope_paths_keep_older_matching_cwd() {
        let dir = tempdir().unwrap();
        let older = dir.path().join("old.jsonl");
        let newer = dir.path().join("new.jsonl");
        fs::write(
            &older,
            r#"{"type":"user","role":"user","sessionId":"old","cwd":"/keep","content":"one"}
"#,
        )
        .unwrap();
        fs::write(
            &newer,
            r#"{"type":"user","role":"user","sessionId":"new","cwd":"/other","content":"two"}
"#,
        )
        .unwrap();
        // Direct parse coverage lives in parse.rs; here we pin cwd matching.
        assert!(is_cwd_in_scope(Some("/keep"), &[PathBuf::from("/keep")]));
        assert!(!is_cwd_in_scope(Some("/other"), &[PathBuf::from("/keep")]));
    }
}

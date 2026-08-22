//! Vendor transcript roots. Env overrides match Claude / Codex / Gemini /
//! OpenCode / pi conventions; clients cannot supply these paths.

use std::env;
use std::path::{Path, PathBuf};

use super::types::CliSessionAgentId;

pub fn user_home() -> Option<PathBuf> {
    env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

pub fn claude_projects_dir() -> Option<PathBuf> {
    if let Some(dir) = env::var_os("CLAUDE_CONFIG_DIR") {
        return Some(PathBuf::from(dir).join("projects"));
    }
    user_home().map(|home| home.join(".claude").join("projects"))
}

pub fn default_codex_home() -> Option<PathBuf> {
    env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| user_home().map(|home| home.join(".codex")))
}

pub fn gemini_tmp_dir() -> Option<PathBuf> {
    if let Some(dir) = env::var_os("GEMINI_HOME") {
        return Some(PathBuf::from(dir).join("tmp"));
    }
    user_home().map(|home| home.join(".gemini").join("tmp"))
}

pub fn cursor_projects_dir() -> Option<PathBuf> {
    user_home().map(|home| home.join(".cursor").join("projects"))
}

pub fn opencode_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(dir) = env::var_os("OPENCODE_HOME") {
        roots.push(PathBuf::from(dir));
    }
    if let Some(xdg) = env::var_os("XDG_DATA_HOME") {
        roots.push(PathBuf::from(xdg).join("opencode"));
    }
    if let Some(home) = user_home() {
        roots.push(home.join(".local").join("share").join("opencode"));
    }
    roots
}

pub fn pi_sessions_dir() -> Option<PathBuf> {
    env::var_os("PI_CODING_AGENT_DIR")
        .map(PathBuf::from)
        .or_else(|| user_home().map(|home| home.join(".pi").join("agent").join("sessions")))
}

pub fn roots_for_agent(agent: CliSessionAgentId) -> Vec<PathBuf> {
    match agent {
        CliSessionAgentId::ClaudeCode => claude_projects_dir().into_iter().collect(),
        CliSessionAgentId::Codex => default_codex_home()
            .map(|home| home.join("sessions"))
            .into_iter()
            .collect(),
        CliSessionAgentId::GeminiCli => gemini_tmp_dir().into_iter().collect(),
        CliSessionAgentId::Cursor => cursor_projects_dir().into_iter().collect(),
        CliSessionAgentId::Opencode => opencode_roots()
            .into_iter()
            .flat_map(|root| [root.join("storage"), root])
            .collect(),
        CliSessionAgentId::Pi => pi_sessions_dir().into_iter().collect(),
    }
}

pub fn is_under_dir(path: &Path, root: &Path) -> bool {
    path.starts_with(root)
}

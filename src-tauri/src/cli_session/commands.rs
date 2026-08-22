//! Tauri IPC for CLI session discovery.

use super::{list_cli_sessions, CliSessionListArgs, CliSessionListResult};

#[tauri::command]
pub async fn list_cli_sessions_cmd(args: Option<CliSessionListArgs>) -> Result<CliSessionListResult, String> {
    let args = args.unwrap_or_default();
    log::info!(
        target: "termul::cli_session",
        "operation=list_cli_sessions_cmd scope_paths={}",
        args.scope_paths.as_ref().map(Vec::len).unwrap_or(0)
    );
    Ok(tokio::task::spawn_blocking(move || list_cli_sessions(args, None))
        .await
        .map_err(|err| format!("cli session scan join failed: {err}"))?)
}

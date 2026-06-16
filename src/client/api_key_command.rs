use anyhow::{anyhow, bail, Result};
use chrono::Utc;
use std::process::Command;

use super::access_token::{get_access_token, is_valid_access_token, set_access_token};
use crate::utils::SHELL;

/// Default expiry duration in seconds (1 hour) if not specified.
const DEFAULT_EXPIRES_IN: i64 = 3600;

/// Execute an api_key_command via the system shell and return the trimmed stdout.
fn run_api_key_command(cmd: &str) -> Result<String> {
    let output = Command::new(&SHELL.cmd)
        .arg(&SHELL.arg)
        .arg(cmd)
        .output()
        .map_err(|e| anyhow!("Failed to execute api_key_command: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("api_key_command exited with {}: {}", output.status, stderr.trim());
    }

    let token = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if token.is_empty() {
        bail!("api_key_command returned empty output");
    }

    Ok(token)
}

/// Resolve an API key using a fallback chain:
/// 1. Static key (from env var or config) — return immediately if available
/// 2. Cached token — return if still valid
/// 3. Execute api_key_command — cache result and return
pub fn resolve_api_key(
    client_name: &str,
    static_key_result: Result<String>,
    api_key_command: Option<&str>,
    expires_in: Option<u64>,
) -> Result<String> {
    // 1. Static key takes priority; otherwise capture the error
    let original_err = match static_key_result {
        Ok(key) => return Ok(key),
        Err(e) => e,
    };

    // 2. No command configured — propagate original error
    let cmd = match api_key_command {
        Some(cmd) => cmd,
        None => return Err(original_err),
    };

    // 3. Check cache
    if is_valid_access_token(client_name) {
        return get_access_token(client_name);
    }

    // 4. Execute command
    let token = run_api_key_command(cmd)?;

    // 5. Cache with expiry
    let expires_in_secs = expires_in.map(|v| v as i64).unwrap_or(DEFAULT_EXPIRES_IN);
    let expires_at = Utc::now().timestamp() + expires_in_secs;
    set_access_token(client_name, token.clone(), expires_at);

    // 6. Return token
    Ok(token)
}

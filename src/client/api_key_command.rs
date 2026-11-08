use anyhow::{anyhow, bail, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use chrono::Utc;
use serde_json::Value;
use std::process::Command;

use super::access_token::{get_access_token, is_valid_access_token, set_access_token};
use crate::utils::SHELL;

/// Default expiry duration in seconds (1 hour) if not specified.
const DEFAULT_EXPIRES_IN: i64 = 3600;
/// Safety margin (seconds) subtracted from a JWT's `exp` claim when caching.
const JWT_EXP_SKEW_SECS: i64 = 60;

/// If `token` is a JWT with a numeric `exp` claim, return `exp - JWT_EXP_SKEW_SECS`.
pub(crate) fn extract_jwt_expires_at(token: &str) -> Option<i64> {
    let mut parts = token.split('.');
    let _header = parts.next()?;
    let payload_b64 = parts.next()?;
    let _sig = parts.next()?;
    let decoded = URL_SAFE_NO_PAD
        .decode(payload_b64.trim_end_matches('='))
        .ok()?;
    let json: Value = serde_json::from_slice(&decoded).ok()?;
    let exp = json.get("exp")?.as_i64()?;
    Some(exp - JWT_EXP_SKEW_SECS)
}

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

    // 4. Execute command with retry (3 attempts, 200ms delay)
    let token = {
        let mut last_err = anyhow!("api_key_command failed after 3 attempts");
        let mut success = None;
        for attempt in 1..=3u32 {
            match run_api_key_command(cmd) {
                Ok(t) => {
                    success = Some(t);
                    break;
                }
                Err(e) => {
                    eprintln!(
                        "[aichat:warn] api_key_command for '{}': attempt {}/3 failed: {}",
                        client_name, attempt, e
                    );
                    last_err = e;
                    if attempt < 3 {
                        std::thread::sleep(std::time::Duration::from_millis(200));
                    }
                }
            }
        }
        match success {
            Some(t) => t,
            None => {
                eprintln!(
                    "[aichat:warn] api_key_command for '{}': all 3 attempts exhausted, giving up",
                    client_name
                );
                return Err(last_err);
            }
        }
    };

    // 5. Cache with expiry: honor JWT `exp` (with skew) so pre-aged tokens never
    // outlive their real lifetime in memory; cap by explicit `expires_in` if given.
    let now = Utc::now().timestamp();
    let fallback_expires_at = now + expires_in.map(|v| v as i64).unwrap_or(DEFAULT_EXPIRES_IN);
    let expires_at = match extract_jwt_expires_at(&token) {
        Some(jwt_exp) if expires_in.is_some() => jwt_exp.min(fallback_expires_at),
        Some(jwt_exp) => jwt_exp,
        None => fallback_expires_at,
    };
    set_access_token(client_name, token.clone(), expires_at);

    // 6. Return token
    Ok(token)
}

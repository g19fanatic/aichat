#!/usr/bin/env bash
# test-multiturn-cache.sh — Integration test for multi-turn prompt cache effectiveness
#
# Verifies that sequential requests with growing conversation history produce
# increasing cache_read_input_tokens and decreasing cache_creation_input_tokens,
# confirming the multi-turn cache breakpoint strategy works correctly.
#
# Prerequisites:
#   - aichat binary on PATH with valid Claude API configuration
#   - A Claude model configured (defaults to claude-sonnet-4-20250514)
#
# Usage:
#   ./scripts/test-multiturn-cache.sh [model_name]
#   MODEL=claude-sonnet-4-20250514 ./scripts/test-multiturn-cache.sh
#
# Exit codes:
#   0 — All cache trend assertions passed
#   1 — Test failed (cache reads did not increase across turns)
#   2 — Setup error (missing binary, bad config, etc.)

set -euo pipefail

# --- Configuration ---
MODEL="${MODEL:-${1:-claude-sonnet-4-20250514}}"
ROLE="${ROLE:-}"  # Optional: --role flag
SLEEP_BETWEEN="${SLEEP_BETWEEN:-3}"  # Seconds between requests for cache registration
TMPDIR_BASE="${TMPDIR:-/tmp}"
WORKDIR=$(mktemp -d "${TMPDIR_BASE}/aichat-cache-test.XXXXXX")
trap 'rm -rf "$WORKDIR"' EXIT

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# --- Helpers ---
log_info()  { echo -e "${BLUE}[INFO]${NC} $*"; }
log_pass()  { echo -e "${GREEN}[PASS]${NC} $*"; }
log_fail()  { echo -e "${RED}[FAIL]${NC} $*"; }
log_warn()  { echo -e "${YELLOW}[WARN]${NC} $*"; }

# --- Prerequisite Checks ---
log_info "Checking prerequisites..."

if ! command -v aichat &>/dev/null; then
    log_fail "aichat binary not found on PATH"
    exit 2
fi

# Verify model is accessible
if ! aichat --list-models 2>/dev/null | grep -q "${MODEL}"; then
    log_warn "Model '${MODEL}' not found in model list (may still work if configured)"
fi

log_info "Using model: ${MODEL}"
log_info "Working directory: ${WORKDIR}"

# --- Generate Stable Content (must exceed 2048 tokens for Anthropic cache minimum) ---
# This simulates the "buffers" field from vim-llm-assistant with realistic code content.
STABLE_CONTENT=$(cat <<'CONTENT_EOF'
============ FILE: src/auth/middleware.rs ============
use axum::{middleware::Next, http::Request, response::Response, extract::State};
use jsonwebtoken::{decode, DecodingKey, Validation, Algorithm};
use crate::{AppState, error::AppError};
use std::sync::Arc;

/// JWT claims structure for authentication tokens.
/// Validates token expiry, issuer, and audience automatically.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct Claims {
    pub sub: String,        // User ID
    pub exp: u64,          // Expiration timestamp
    pub iat: u64,          // Issued at timestamp
    pub iss: String,       // Issuer
    pub aud: Vec<String>,  // Audience
    pub roles: Vec<String>, // User roles for RBAC
}

/// Authentication middleware that validates JWT tokens from the Authorization header.
/// Returns 401 Unauthorized if the token is missing, expired, or invalid.
/// On success, inserts the validated Claims into request extensions for handlers.
pub async fn require_auth<B>(
    State(state): State<Arc<AppState>>,
    mut req: Request<B>,
    next: Next<B>,
) -> Result<Response, AppError> {
    let auth_header = req.headers()
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .ok_or(AppError::Unauthorized("Missing Authorization header".into()))?;

    let token = auth_header
        .strip_prefix("Bearer ")
        .ok_or(AppError::Unauthorized("Invalid Authorization format".into()))?;

    let key = DecodingKey::from_secret(state.jwt_secret.as_bytes());
    let mut validation = Validation::new(Algorithm::HS256);
    validation.set_issuer(&["auth-service"]);
    validation.set_audience(&["api"]);

    let claims = decode::<Claims>(token, &key, &validation)
        .map_err(|e| AppError::Unauthorized(format!("Token validation failed: {}", e)))?
        .claims;

    req.extensions_mut().insert(claims);
    Ok(next.run(req).await)
}

/// Role-based access control middleware factory.
/// Creates a middleware that checks if the authenticated user has any of the required roles.
pub fn require_roles(roles: &[&str]) -> impl Fn(Claims) -> Result<(), AppError> + Clone {
    let required: Vec<String> = roles.iter().map(|s| s.to_string()).collect();
    move |claims: Claims| {
        if claims.roles.iter().any(|r| required.contains(r)) {
            Ok(())
        } else {
            Err(AppError::Forbidden(format!(
                "Required roles: {:?}, user has: {:?}",
                required, claims.roles
            )))
        }
    }
}

============ FILE: src/auth/refresh.rs ============
use std::sync::Arc;
use tokio::sync::Mutex;
use crate::{AppState, error::AppError};

/// Token refresh state machine with mutex protection against race conditions.
/// Ensures only one refresh operation runs concurrently per user session.
pub struct TokenRefresher {
    state: Arc<AppState>,
    lock: Arc<Mutex<()>>,
    max_retries: u32,
}

impl TokenRefresher {
    pub fn new(state: Arc<AppState>) -> Self {
        Self {
            state,
            lock: Arc::new(Mutex::new(())),
            max_retries: 3,
        }
    }

    /// Attempt to refresh an expired token. Uses a mutex to prevent concurrent
    /// refresh attempts which could invalidate each other.
    pub async fn refresh(&self, refresh_token: &str) -> Result<TokenPair, AppError> {
        let _guard = self.lock.lock().await;

        // Validate refresh token hasn't been revoked
        if self.state.revoked_tokens.contains(refresh_token) {
            return Err(AppError::Unauthorized("Refresh token revoked".into()));
        }

        // Issue new token pair
        let user_id = self.extract_user_id(refresh_token)?;
        let new_pair = self.state.token_issuer.issue_pair(&user_id).await?;

        // Revoke old refresh token (rotation)
        self.state.revoked_tokens.insert(refresh_token.to_string());

        Ok(new_pair)
    }

    fn extract_user_id(&self, token: &str) -> Result<String, AppError> {
        // Decode without validation (we only need the sub claim)
        let key = DecodingKey::from_secret(self.state.jwt_secret.as_bytes());
        let mut validation = Validation::new(Algorithm::HS256);
        validation.validate_exp = false; // Allow expired tokens for refresh
        let data = decode::<Claims>(token, &key, &validation)
            .map_err(|e| AppError::Unauthorized(format!("Invalid refresh token: {}", e)))?;
        Ok(data.claims.sub)
    }
}

/// Represents an access/refresh token pair returned after authentication.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TokenPair {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_in: u64,
    pub token_type: String,
}

============ FILE: src/handlers/users.rs ============
use axum::{extract::{Path, State, Query}, Json, response::IntoResponse};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use crate::{AppState, auth::Claims, error::AppError};

#[derive(Debug, Deserialize)]
pub struct ListUsersQuery {
    pub page: Option<u32>,
    pub per_page: Option<u32>,
    pub search: Option<String>,
    pub role: Option<String>,
    pub sort_by: Option<String>,
    pub sort_dir: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct UserResponse {
    pub id: String,
    pub email: String,
    pub name: String,
    pub roles: Vec<String>,
    pub created_at: String,
    pub last_login: Option<String>,
    pub is_active: bool,
}

#[derive(Debug, Serialize)]
pub struct PaginatedResponse<T> {
    pub data: Vec<T>,
    pub total: u64,
    pub page: u32,
    pub per_page: u32,
    pub total_pages: u32,
}

/// List users with pagination and filtering.
/// Requires admin role for full access, or returns only the authenticated user's data.
pub async fn list_users(
    State(state): State<Arc<AppState>>,
    claims: Claims,
    Query(query): Query<ListUsersQuery>,
) -> Result<impl IntoResponse, AppError> {
    let page = query.page.unwrap_or(1);
    let per_page = query.per_page.unwrap_or(20).min(100);
    let offset = (page - 1) * per_page;

    let is_admin = claims.roles.contains(&"admin".to_string());

    let (users, total) = if is_admin {
        state.user_repo.list_all(offset, per_page, query.search.as_deref(), query.role.as_deref()).await?
    } else {
        let user = state.user_repo.find_by_id(&claims.sub).await?
            .ok_or(AppError::NotFound("User not found".into()))?;
        (vec![user], 1)
    };

    let response = PaginatedResponse {
        data: users.into_iter().map(|u| UserResponse {
            id: u.id,
            email: u.email,
            name: u.name,
            roles: u.roles,
            created_at: u.created_at.to_rfc3339(),
            last_login: u.last_login.map(|t| t.to_rfc3339()),
            is_active: u.is_active,
        }).collect(),
        total,
        page,
        per_page,
        total_pages: ((total as f64) / (per_page as f64)).ceil() as u32,
    };

    Ok(Json(response))
}

/// Get a specific user by ID. Admin can view any user; others only themselves.
pub async fn get_user(
    State(state): State<Arc<AppState>>,
    claims: Claims,
    Path(user_id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    if user_id != claims.sub && !claims.roles.contains(&"admin".to_string()) {
        return Err(AppError::Forbidden("Cannot access other users".into()));
    }

    let user = state.user_repo.find_by_id(&user_id).await?
        .ok_or(AppError::NotFound(format!("User {} not found", user_id)))?;

    Ok(Json(UserResponse {
        id: user.id,
        email: user.email,
        name: user.name,
        roles: user.roles,
        created_at: user.created_at.to_rfc3339(),
        last_login: user.last_login.map(|t| t.to_rfc3339()),
        is_active: user.is_active,
    }))
}
CONTENT_EOF
)

# --- Build JSON Input Files ---
# Each file has growing llm_history_turns while keeping stable_content the same.
# This simulates a multi-turn conversation where the context (buffers) remains constant
# but the conversation history grows.

build_json_input() {
    local num_turns="$1"
    local output_file="$2"

    # Build history turns array
    local turns="[]"
    if [ "$num_turns" -ge 1 ]; then
        turns='[{"user":"Explain the authentication middleware flow in this codebase. How does the JWT validation work and what happens when a token expires?","assistant":"The auth middleware in src/auth/middleware.rs validates JWT tokens from the Authorization header. It extracts the Bearer token, validates it against the configured secret using HS256, checks issuer (auth-service) and audience (api) claims, and inserts the validated Claims struct into request extensions. Expired tokens return 401 Unauthorized. The Claims struct includes sub (user ID), exp, iat, iss, aud, and roles fields for RBAC."}]'
    fi
    if [ "$num_turns" -ge 2 ]; then
        turns='[{"user":"Explain the authentication middleware flow in this codebase. How does the JWT validation work and what happens when a token expires?","assistant":"The auth middleware in src/auth/middleware.rs validates JWT tokens from the Authorization header. It extracts the Bearer token, validates it against the configured secret using HS256, checks issuer (auth-service) and audience (api) claims, and inserts the validated Claims struct into request extensions. Expired tokens return 401 Unauthorized. The Claims struct includes sub (user ID), exp, iat, iss, aud, and roles fields for RBAC."},{"user":"How does the token refresh mechanism handle race conditions? I see there is a mutex but want to understand the full flow.","assistant":"The TokenRefresher in src/auth/refresh.rs uses a tokio::sync::Mutex to serialize refresh attempts. When refresh() is called: (1) it acquires the mutex lock, preventing concurrent refreshes; (2) checks if the refresh token has been revoked; (3) extracts the user_id from the token (without expiry validation since refresh tokens may be expired); (4) issues a new token pair via the token_issuer; (5) revokes the old refresh token (rotation pattern). The mutex ensures that if two requests try to refresh simultaneously, only one succeeds and the other gets a revoked-token error."}]'
    fi
    if [ "$num_turns" -ge 3 ]; then
        turns='[{"user":"Explain the authentication middleware flow in this codebase. How does the JWT validation work and what happens when a token expires?","assistant":"The auth middleware in src/auth/middleware.rs validates JWT tokens from the Authorization header. It extracts the Bearer token, validates it against the configured secret using HS256, checks issuer (auth-service) and audience (api) claims, and inserts the validated Claims struct into request extensions. Expired tokens return 401 Unauthorized. The Claims struct includes sub (user ID), exp, iat, iss, aud, and roles fields for RBAC."},{"user":"How does the token refresh mechanism handle race conditions? I see there is a mutex but want to understand the full flow.","assistant":"The TokenRefresher in src/auth/refresh.rs uses a tokio::sync::Mutex to serialize refresh attempts. When refresh() is called: (1) it acquires the mutex lock, preventing concurrent refreshes; (2) checks if the refresh token has been revoked; (3) extracts the user_id from the token (without expiry validation since refresh tokens may be expired); (4) issues a new token pair via the token_issuer; (5) revokes the old refresh token (rotation pattern). The mutex ensures that if two requests try to refresh simultaneously, only one succeeds and the other gets a revoked-token error."},{"user":"What about the list_users endpoint - how does the RBAC interact with pagination? Can a non-admin user see other users data?","assistant":"In src/handlers/users.rs, list_users checks claims.roles for admin. Non-admin users only see their own record (the repo returns a single-element vec). Admins get full paginated results with optional search, role filter, and sort. The per_page is capped at 100 to prevent abuse. get_user similarly checks ownership: non-admins can only access their own user_id. Both return 403 Forbidden for unauthorized cross-user access."}]'
    fi

    # Escape stable content for JSON (handle newlines, quotes, backslashes)
    local escaped_content
    escaped_content=$(printf '%s' "$STABLE_CONTENT" | python3 -c 'import sys,json; print(json.dumps(sys.stdin.read()))')

    # Write the full JSON input file
    cat > "$output_file" <<JSON_EOF
{
  "llm_history_turns": ${turns},
  "buffers": ${escaped_content},
  "_cache_hints": {
    "breakpoint_after": ["buffers"],
    "stable_fields": ["buffers"],
    "dynamic_fields": ["prompt"]
  },
  "prompt": "Based on the codebase, suggest improvements to the error handling. Keep your answer to 1 sentence."
}
JSON_EOF
}

# --- Execute Requests ---
declare -a CACHE_READ=()
declare -a CACHE_WRITTEN=()

run_request() {
    local turn_num="$1"
    local json_file="${WORKDIR}/input_turn${turn_num}.json"

    build_json_input "$turn_num" "$json_file"

    log_info "Request ${turn_num}: ${turn_num} history turn(s)..."

    # Build aichat command
    local cmd="AICHAT_MULTITURN_READY=1 aichat --model ${MODEL}"
    if [ -n "${ROLE}" ]; then
        cmd="${cmd} --role ${ROLE}"
    fi
    cmd="${cmd} -S --file ${json_file} -- 'Respond in one word: yes'"

    # Execute and capture stderr (where cache metrics appear)
    local stderr_file="${WORKDIR}/stderr_turn${turn_num}.txt"
    local stdout_file="${WORKDIR}/stdout_turn${turn_num}.txt"

    # Run aichat, capturing stdout and stderr separately
    eval "${cmd}" > "$stdout_file" 2> "$stderr_file" || true

    # Parse cache metrics from stderr
    # Format: 📦 Cache: N read, N written
    local read_tokens=0
    local written_tokens=0

    if grep -q "Cache:" "$stderr_file" 2>/dev/null; then
        read_tokens=$(grep "Cache:" "$stderr_file" | grep -oP '\d+ read' | grep -oP '\d+' || echo "0")
        written_tokens=$(grep "Cache:" "$stderr_file" | grep -oP '\d+ written' | grep -oP '\d+' || echo "0")
    fi

    # Handle empty values
    read_tokens="${read_tokens:-0}"
    written_tokens="${written_tokens:-0}"

    CACHE_READ+=("$read_tokens")
    CACHE_WRITTEN+=("$written_tokens")

    log_info "  → cache_read=${read_tokens}, cache_creation=${written_tokens}"

    # Show any errors
    if grep -qi "error\|fail" "$stderr_file" 2>/dev/null | grep -v "Cache:" ; then
        log_warn "  stderr: $(cat "$stderr_file")"
    fi
}

# --- Main Test Execution ---
echo ""
echo "═══════════════════════════════════════════════════════════════"
echo "  Multi-Turn Cache Integration Test"
echo "  Model: ${MODEL}"
echo "  Strategy: 3 sequential requests with growing history"
echo "═══════════════════════════════════════════════════════════════"
echo ""

# Request 1: 1 history turn (cold cache — everything gets written)
run_request 1

log_info "Sleeping ${SLEEP_BETWEEN}s for cache registration..."
sleep "$SLEEP_BETWEEN"

# Request 2: 2 history turns (turn 1 cached — should see higher reads)
run_request 2

log_info "Sleeping ${SLEEP_BETWEEN}s for cache registration..."
sleep "$SLEEP_BETWEEN"

# Request 3: 3 history turns (turns 1-2 cached — should see even higher reads)
run_request 3

# --- Validate Results ---
echo ""
echo "═══════════════════════════════════════════════════════════════"
echo "  Results Summary"
echo "═══════════════════════════════════════════════════════════════"
echo ""

printf "  %-10s %15s %15s\n" "Request" "Cache Read" "Cache Written"
printf "  %-10s %15s %15s\n" "-------" "----------" "-------------"
for i in 0 1 2; do
    printf "  %-10s %15s %15s\n" "Turn $((i+1))" "${CACHE_READ[$i]}" "${CACHE_WRITTEN[$i]}"
done
echo ""

# --- Assertions ---
PASSED=0
FAILED=0

assert_gt() {
    local desc="$1" val_a="$2" val_b="$3"
    if [ "$val_a" -gt "$val_b" ] 2>/dev/null; then
        log_pass "$desc ($val_a > $val_b)"
        ((PASSED++))
    else
        log_fail "$desc (expected $val_a > $val_b)"
        ((FAILED++))
    fi
}

assert_ge() {
    local desc="$1" val_a="$2" val_b="$3"
    if [ "$val_a" -ge "$val_b" ] 2>/dev/null; then
        log_pass "$desc ($val_a >= $val_b)"
        ((PASSED++))
    else
        log_fail "$desc (expected $val_a >= $val_b)"
        ((FAILED++))
    fi
}

# Core assertion: cache reads should increase as history grows
# Turn 2 should read more than Turn 1 (history turn 1 is now cached)
assert_gt "Turn 2 cache_read > Turn 1 cache_read" "${CACHE_READ[1]}" "${CACHE_READ[0]}"

# Turn 3 should read more than Turn 2 (history turns 1-2 are now cached)
assert_ge "Turn 3 cache_read >= Turn 2 cache_read" "${CACHE_READ[2]}" "${CACHE_READ[1]}"

# Cache creation should decrease or stay low after first request
# (Most content gets written on turn 1, subsequent turns only write new content)
assert_gt "Turn 1 cache_written > Turn 2 cache_written (less new content)" \
    "${CACHE_WRITTEN[0]}" "${CACHE_WRITTEN[1]}"

# Sanity check: first request should have written SOMETHING to cache
assert_gt "Turn 1 cache_written > 0 (cache was populated)" "${CACHE_WRITTEN[0]}" "0"

echo ""
echo "═══════════════════════════════════════════════════════════════"
if [ "$FAILED" -eq 0 ]; then
    log_pass "All ${PASSED} assertions passed! Multi-turn caching is working correctly."
    echo ""
    echo "  Cache reads grow with conversation history, confirming that:"
    echo "  1. System prompt + tools are cached across all turns"
    echo "  2. History turn boundaries get cache breakpoints"
    echo "  3. New turns reuse the cached conversation prefix"
    echo ""
    exit 0
else
    log_fail "${FAILED} assertion(s) failed, ${PASSED} passed."
    echo ""
    echo "  Possible causes:"
    echo "  - Content below Anthropic's 2048-token cache minimum"
    echo "  - Cache not yet available (retry after 5+ seconds)"
    echo "  - Model doesn't support prompt caching"
    echo "  - API returned error (check stderr files in ${WORKDIR})"
    echo ""
    echo "  Debug: stderr files preserved at ${WORKDIR}/stderr_turn*.txt"
    # Don't remove workdir on failure for debugging
    trap '' EXIT
    exit 1
fi

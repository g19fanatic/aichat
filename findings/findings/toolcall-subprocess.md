# Task 4: Subprocess Spawn Costs in `run_llm_function`
**Task ID**: 4  **Scope**: Per-call overhead of `run_llm_function` — process spawn, PATH reconstruction, UUID+temp-file IPC, env HashMap clone, output read-back, double-parse of JSON output.

---

## Evidence Gathered

### 1. `run_command` uses blocking `std::process::Command` — one fork+exec per tool call
`src/utils/command.rs:81-91`:
```rust
pub fn run_command<T: AsRef<OsStr>>(
    cmd: &str, args: &[T], envs: Option<HashMap<String, String>>,
) -> Result<i32> {
    let status = Command::new(cmd)
        .args(args.iter())
        .envs(envs.unwrap_or_default())
        .status()?;
    Ok(status.code().unwrap_or_default())
}
```
- Uses `std::process::Command` → **blocking** fork+exec.
- `.envs()` merges the supplied HashMap ADDITIVELY with the child's inherited environment (does NOT clear parent env). All parent env vars are inherited.
- Returns only exit code; stdout of the child goes directly to the terminal.
- `.status()` blocks until the child exits — no async, no polling.

### 2. PATH string rebuilt on every call
`src/function.rs:283-295`:
```rust
let mut bin_dirs: Vec<PathBuf> = vec![];
if cmd_args.len() > 1 {
    let dir = Config::agent_functions_dir(&cmd_name).join("bin");
    if dir.exists() { bin_dirs.push(dir); }
}
bin_dirs.push(Config::functions_bin_dir());
let current_path = std::env::var("PATH").context("No PATH environment variable")?;
let prepend_path = bin_dirs
    .iter()
    .map(|v| format!("{}{PATH_SEP}", v.display()))
    .collect::<Vec<_>>()
    .join("");
envs.insert("PATH".into(), format!("{prepend_path}{current_path}"));
```
- `env::var("PATH")` syscall per call.
- `Config::functions_bin_dir()` called every invocation: `src/config/mod.rs:378`: `Self::functions_dir().join(FUNCTIONS_BIN_DIR_NAME)` → calls `functions_dir()` which itself does `env::var(get_env_name("functions_dir"))` + PathBuf construction.
- `Config::agent_functions_dir(&cmd_name)` (for agent sub-calls): `src/config/mod.rs:422-426`: `format!("{}_FUNCTIONS_DIR", normalize_env_name(name))` + `env::var()` per call + potential `Self::agents_functions_dir().join(name)` PathBuf.
- Multiple String allocs + Vec<PathBuf> allocation + format! calls just to build PATH.
- PATH does not change between calls within a session — this whole block is 100% redundant after the first call.

### 3. UUID-based temp path generated per call (no file creation)
`src/utils/mod.rs:194-201`:
```rust
pub fn temp_file(prefix: &str, suffix: &str) -> PathBuf {
    env::temp_dir().join(format!(
        "{}-{}{prefix}{}{suffix}",
        env!("CARGO_CRATE_NAME").to_lowercase(),
        process::id(),
        uuid::Uuid::new_v4()
    ))
}
```
- `uuid::Uuid::new_v4()` calls `getrandom` syscall (CSPRNG) each time.
- `format!(...)` + `env::temp_dir()` + `.join(...)` → String alloc + PathBuf alloc.
- Called at `src/function.rs:302`: `let temp = temp_file("-eval-", "")`.
- **The file itself is NOT created here** — only the path is generated. The tool script creates and writes to this path.

### 4. `envs` HashMap is cloned before being passed to subprocess — unnecessary
`src/function.rs:321`:
```rust
let exit_code = run_command(&cmd_name, &cmd_args, Some(envs.clone()))
    .map_err(|err| anyhow!("Unable to run {cmd_name}, {err}"))?;
```
- `envs` is a `mut HashMap<String, String>` that is **never used again after line 321**.
- Yet it is `.clone()`d instead of moved.
- For an agent with N variables, `variable_envs()` (agent.rs:255) creates a HashMap of N entries (each `format!("LLM_AGENT_VAR_...")` + `v.clone()`), which is then cloned again here. Double allocation.
- `run_command` takes `envs: Option<HashMap<String, String>>` by value, so a move is semantically valid.

### 5. `variable_envs()` allocates a fresh HashMap on every tool call for agent functions
`src/config/agent.rs:255-263`:
```rust
pub fn variable_envs(&self) -> HashMap<String, String> {
    self.variables()
        .iter()
        .map(|(k, v)| {
            (
                format!("LLM_AGENT_VAR_{}", normalize_env_name(k)),
                v.clone(),
            )
        })
        .collect()
}
```
- Called from `extract_call_config_from_agent` at `src/config/agent.rs:318` on every tool call that dispatches an agent sub-function.
- N agent variables → N × `format!()` + N × `v.clone()` allocations per tool call.
- Agent variables do NOT change between tool calls in a session — this HashMap could be cached.

### 6. Output read-back: stat + read_to_string syscalls
`src/function.rs:340-355`:
```rust
} else if temp_file.exists() {         // <- stat() syscall
    let contents =
        fs::read_to_string(temp_file).context(...)?;  // <- open() + read() syscalls
    if !contents.is_empty() {
        if contents.trim().starts_with('{') || contents.trim().starts_with('[') {
            match jsonic::parse(&contents) {  // <- first parse pass
                Ok(json_item) => {
                    output = Some(json_item.as_str().unwrap_or(&contents).to_string()); // <- new String alloc
                }
                ...
            }
        } else {
            output = Some(contents);
        }
    }
}
```
- After the subprocess exits: `temp_file.exists()` is one `stat()` call.
- `fs::read_to_string()` is another `open()` + `read()` → the entire output materialized in RAM as a String.
- **No temp file cleanup**: the file in `/tmp` is never deleted by aichat. Files accumulate across the session. On a very long session with many tool calls, this leaks disk space. OS temp dir cleanup is the only reclamation path.

### 7. Double-parse of JSON output: jsonic → String → serde_json
At `src/function.rs:348-353` (first parse, in `run_llm_function`):
```rust
match jsonic::parse(&contents) {
    Ok(json_item) => {
        output = Some(json_item.as_str().unwrap_or(&contents).to_string());
    }
```
Then at `src/function.rs:213` (second parse, in `ToolCall::eval`):
```rust
let output = match run_llm_function(cmd_name, cmd_args, envs)? {
    Some(contents) => serde_json::from_str(&contents)
        .ok()
        .unwrap_or_else(|| json!({"output": contents})),
    None => Value::Null,
};
```
- For JSON output: **jsonic::parse** (pass 1) → `as_str()` → **owned String** (new alloc) → **serde_json::from_str** (pass 2).
- The jsonic pass normalizes potentially malformed JSON from tool scripts. The serde_json pass deserializes into `Value`.
- For well-formed JSON output these could be merged: parse once with serde_json (and if it fails, fallback to jsonic then serde_json). Or jsonic could produce `serde_json::Value` directly if such an API exists.

### 8. LLM_OUTPUT environment variable looked up twice
`src/function.rs:298` (first lookup):
```rust
let llm_output_defined = std::env::var("LLM_OUTPUT").is_ok();
```
`src/function.rs:331` (second lookup, inside `if llm_output_defined` branch):
```rust
if let Ok(llm_path) = std::env::var("LLM_OUTPUT") {
```
- When `LLM_OUTPUT` is already defined (nested call scenario), the env var is looked up twice. Cheap but unnecessary — the value from line 298 could be captured as `Option<String>` and reused.

### 9. `run_command_with_output` exists but is NOT used for tool calls
`src/utils/command.rs:93-103`:
```rust
pub fn run_command_with_output<T: AsRef<OsStr>>(
    cmd: &str, args: &[T], envs: Option<HashMap<String, String>>,
) -> Result<(bool, String, String)> {
    let output = Command::new(cmd)
        .args(args.iter())
        .envs(envs.unwrap_or_default())
        .output()?;
    ...
    Ok((status.success(), stdout.to_string(), stderr.to_string()))
}
```
- Captures stdout into a Rust String — a direct alternative to the temp-file IPC.
- Currently NOT used for tool calls because the IPC design sends structured output via `LLM_OUTPUT` file, while stdout goes to the user terminal.
- Switching to stdout capture would require: tool scripts write to stdout (structured JSON), visible user output moves to stderr. This is a breaking change to the tool contract.

---

## Findings

1. **OS process spawn is the dominant per-call cost** (CONFIRMED). Every tool call does one `fork+exec` via blocking `Command::new().status()` at `src/utils/command.rs:88`. On Linux, `fork+exec` for a shell script costs ~1–5ms. For a Python script, ~20–80ms (interpreter startup). This dwarfs all other per-call costs combined.

2. **PATH string is rebuilt identically on every call** (CONFIRMED). The code at `src/function.rs:283-295` reads `env::var("PATH")`, calls `Config::functions_bin_dir()` and (for agents) `Config::agent_functions_dir()` on every invocation. PATH doesn't change during a session. This is 3–5 syscalls + 3–5 String/PathBuf allocs that could be computed once and reused.

3. **`envs` HashMap is cloned unnecessarily before subprocess call** (CONFIRMED). `src/function.rs:321`: `run_command(&cmd_name, &cmd_args, Some(envs.clone()))` — the HashMap is cloned even though `envs` is not used again. Changing this to a move (`Some(envs)`) eliminates one full HashMap copy per tool call.

4. **`variable_envs()` allocates a fresh HashMap per tool call for agent functions** (CONFIRMED). `src/config/agent.rs:255-263` constructs a `HashMap<String, String>` via `collect()` on every invocation of `ToolCall::eval` for agent sub-functions. Since agent variables are session-stable, this HashMap could be computed once at agent init and cached.

5. **Temp file is never cleaned up** (CONFIRMED). `src/function.rs:302` + `src/function.rs:340-344` — the temp file in `/tmp` is created by the tool script and read back, but `fs::remove_file()` is never called. In long agent sessions with many tool calls, orphaned files accumulate in `/tmp`. No correctness issue (OS will clean temp dir eventually), but adds inode pressure and disk I/O.

6. **Double-parse of JSON output: jsonic + serde_json** (CONFIRMED). For JSON outputs, `run_llm_function` parses with `jsonic::parse` then materializes as a new String; `ToolCall::eval` then parses again with `serde_json::from_str`. Two full parse passes + one intermediate String allocation per JSON-producing tool call.

7. **UUID/CSPRNG call per tool invocation** (CONFIRMED). `src/utils/mod.rs:199`: `uuid::Uuid::new_v4()` calls `getrandom` (a syscall) each time. For the common case where outputs are produced reliably, a simpler counter-based temp name would work and be cheaper.

8. **LLM_OUTPUT env var looked up twice in the pre-defined case** (CONFIRMED). `src/function.rs:298,331` — the value is discarded at line 298 (`is_ok()`) then re-fetched at line 331. Minor but avoidable.

9. **Prompt `format!` even when not printed** (CONFIRMED). `src/function.rs:278`: `let prompt = format!("Call {cmd_name} {}", cmd_args.join(" "))` — creates a heap String on every call. The conditional at line 315 (`if *IS_STDOUT_TERMINAL || llm_output_defined`) means in non-terminal batch runs the String is allocated and immediately dropped. This should be a lazy/deferred format only evaluated if the condition is true.

10. **Tool stdout goes to terminal; IPC is file-based** (CONFIRMED by design). The decision to use `LLM_OUTPUT` file for structured output + stdout for user-visible output is intentional. This makes it impossible to use `run_command_with_output` as a drop-in replacement without changing the tool script contract (stdout → stderr for user output).

---

## Optimization Candidates

| # | Proposal | Impact | Effort | Risk | Evidence Basis |
|---|----------|--------|--------|------|----------------|
| 1 | **Cache PATH+bin_dirs string once per agent session** — compute the PATH prefix in `run_llm_function` or at agent init; store as `OnceLock<String>` or as a field on `Agent`/`Config`. Skip recomputation on every call. | low-med | S | low | CONFIRMED (function.rs:283-295) |
| 2 | **Move `envs` instead of clone at function.rs:321** — change `Some(envs.clone())` to `Some(envs)`. Trivial 1-line change, eliminates one HashMap clone per tool call. | low | XS | none | CONFIRMED (function.rs:321) |
| 3 | **Cache `variable_envs()` as a field on `Agent`** — compute once at agent init, invalidate only if variables are mutated. Eliminates N×format!() + N×clone() per tool call for agent functions. | low-med | S | low | CONFIRMED (agent.rs:255-263, function.rs:318) |
| 4 | **Delete temp file after reading** — add `let _ = fs::remove_file(&temp_file)` after `fs::read_to_string()`. Prevents disk-space accumulation in long sessions. | low (correctness) | XS | none | CONFIRMED (function.rs:340-355) |
| 5 | **Merge double-parse: jsonic+serde_json → single serde_json with jsonic fallback** — attempt `serde_json::from_str` directly; only fall back to jsonic if that fails. Eliminates one parse pass + one intermediate String allocation for well-formed JSON outputs (common case). | low | S | low | CONFIRMED (function.rs:348-353, ToolCall::eval:213) |
| 6 | **Lazy prompt format** — wrap `format!("Call {cmd_name}...")` inside the `if *IS_STDOUT_TERMINAL || llm_output_defined` block to avoid heap alloc in non-terminal batch runs. | negligible | XS | none | CONFIRMED (function.rs:278,315) |
| 7 | **Capture LLM_OUTPUT env value once** — change `std::env::var("LLM_OUTPUT").is_ok()` at line 298 to `std::env::var("LLM_OUTPUT").ok()` and reuse the `Option<String>` at line 331, eliminating the second lookup. | negligible | XS | none | CONFIRMED (function.rs:298,331) |
| 8 | **Parallelize `eval_tool_calls` loop** (covered by Task 3) — the multiple tool calls per round could run concurrently. For N calls each taking T ms, reduces latency from N×T to max(T). The PATH caching above is a prerequisite for safe parallelism. | **HIGH** | M | med | CONFIRMED (Task 3 research) |
| 9 | **Long-lived worker processes** — replace fork+exec per call with a persistent worker process per tool script, communicating via stdin/stdout pipes. Eliminates all process spawn overhead (~95% of per-call cost). | **HIGH** | XL | high | THEORY — no infrastructure exists for this |
| 10 | **Replace UUID with atomic counter for temp names** — `AtomicU64` counter instead of `uuid::Uuid::new_v4()` for temp file naming. Eliminates `getrandom` syscall per call. Uniqueness is still guaranteed (PID + counter). | negligible | XS | low | CONFIRMED (utils/mod.rs:194-201) |

---

## Open Questions / Needs Verification

- **How long does `getrandom` actually take?** On Linux with `/dev/urandom` and VDSO, it may be very fast (< 1µs). If so, option #10 is not worth the change.
- **Is `jsonic::parse` measurably faster or slower than `serde_json::from_str` for the same input?** If serde_json is faster and handles slightly malformed JSON gracefully (via `serde_json::Value` parsing which is lenient), the jsonic pass may be skippable entirely.
- **How often are tool scripts Python vs. shell?** Python startup cost (~20-80ms) makes per-call spawn cost dramatically higher for Python tools; shell scripts spawn in ~1-2ms. This changes the ROI of the long-lived worker approach.
- **Can `run_command_with_output` replace the LLM_OUTPUT mechanism without a tool contract change?** Only if tool scripts redirect their user-visible output to stderr voluntarily or via `exec 2>&1 1>/dev/null`. No existing tool enforcement mechanism.
- **Does `Command::envs()` on Linux actually clear PATH before setting the new one?** Confirmed: `.envs()` is additive, but since PATH is explicitly inserted into the `envs` HashMap, the child process gets the new PATH value overriding the inherited one.
- **What is the actual wall-clock cost of PATH rebuild + UUID gen vs. fork+exec?** PATH rebuild is estimated at ~5–10µs; UUID is ~1–2µs; fork+exec for `/bin/sh` is ~1–3ms. The subprocess spawn is 100–300× more expensive.

---

## Hot-Path Classification

**Per-call** (every tool execution):
- `run_command` fork+exec (dominant cost, CONFIRMED)
- `temp_file()` UUID generation (CONFIRMED)
- PATH string rebuild (CONFIRMED)
- `envs.clone()` (CONFIRMED)
- `variable_envs()` fresh HashMap (for agent functions, CONFIRMED)
- `temp_file.exists()` stat (CONFIRMED)
- `fs::read_to_string()` (CONFIRMED)
- jsonic+serde_json double-parse (for JSON outputs, CONFIRMED)

**Per-session** (one-time costs):
- PATH contents (never changes — only the rebuild is per-call, not the PATH value itself)
- Agent variables (stable — only `variable_envs()` allocation is per-call)

The subprocess spawn (~1–80ms per call) completely dominates all other costs (~10–100µs total). Optimizations to PATH rebuild, envs clone, and variable_envs have correctness and code-cleanliness value but will not be measurably visible on wall-clock benchmarks compared to spawn cost.

# Tool-Call IPC: LLM_OUTPUT Tempfile Mechanism
**Task ID**: 6  **Scope**: Deep-dive on the tempfile/LLM_OUTPUT IPC mechanism — race conditions, fsync behavior, nested-LLM_OUTPUT logic, and alternatives.

## Evidence Gathered

### 1. `temp_file()` implementation
- `src/utils/mod.rs:194-200` — UUID-based path generation, called once per tool call:
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
  Per-call costs: `env::temp_dir()` (getenv/syscall), `process::id()` (O(1)), `uuid::Uuid::new_v4()` (cryptographic RNG draw), `format!()` (heap alloc ~50 bytes). **aichat generates the path; the tool script creates the file.**

### 2. `run_command` — blocks until subprocess exits (NO race condition)
- `src/utils/command.rs:82-89`:
  ```rust
  pub fn run_command<T: AsRef<OsStr>>(
      cmd: &str, args: &[T], envs: Option<HashMap<String, String>>,
  ) -> Result<i32> {
      let status = Command::new(cmd).args(args.iter()).envs(envs.unwrap_or_default()).status()?;
      Ok(status.code().unwrap_or_default())
  }
  ```
  `.status()` blocks until the subprocess exits. POSIX guarantees all file writes are visible after process exit (all fds closed, page cache flushed). **No fsync needed. No race condition.** Read-back happens after `.status()` returns (function.rs lines 325-355 come after line 321's `run_command`).

### 3. LLM_OUTPUT nested detection logic
- `src/function.rs:297-311`:
  ```rust
  let llm_output_defined = std::env::var("LLM_OUTPUT").is_ok();
  let temp_file = if !llm_output_defined {
      let temp = temp_file("-eval-", "");
      envs.insert("LLM_OUTPUT".into(), temp.display().to_string());
      temp
  } else {
      PathBuf::new()  // placeholder
  };
  ```
  Check is against the **current process environment** (not the `envs` HashMap). This means if any parent process already set `LLM_OUTPUT`, aichat will skip creating a temp file and instead read from the inherited path after subprocess exits (function.rs:328-340).

### 4. Double LLM_OUTPUT env lookup
- `src/function.rs:298`: `std::env::var("LLM_OUTPUT").is_ok()` — boolean existence check
- `src/function.rs:331`: `if let Ok(llm_path) = std::env::var("LLM_OUTPUT") {` — re-fetches full value

Two separate `getenv()` syscalls where one suffices. Fix: `let llm_output_env = std::env::var("LLM_OUTPUT").ok();` at line 297, then use `llm_output_env.is_some()` for the bool and `llm_output_env.as_deref()` for the path.

### 5. `envs.clone()` — unnecessary copy of HashMap
- `src/function.rs:319-320`: `let exit_code = run_command(&cmd_name, &cmd_args, Some(envs.clone()))`
- `envs` is never used after this line. The `envs` HashMap holds PATH (large string) + LLM_OUTPUT + agent variables. Should be `Some(envs)` (move). This was also noted in Task 4.

### 6. No temp file cleanup — confirmed globally
- `rg "remove_file" src/ -n` shows hits only in:
  - `src/config/mod.rs:750` — RAG vector cleanup
  - `src/config/mod.rs:1084` — session file cleanup
  - `src/config/mod.rs:1352` — RAG path cleanup
- **Zero hits in `function.rs` or `utils/command.rs`**. Neither the `-eval-` files (tool output) nor `-output-` files (document loader, `run_loader_command`) are ever deleted. Every tool call leaks an orphaned `/tmp/aichat-<PID>-eval-<UUID>` file.

### 7. `run_command_with_output` — stdout capture alternative exists
- `src/utils/command.rs:89-97`:
  ```rust
  pub fn run_command_with_output<T: AsRef<OsStr>>(
      cmd: &str, args: &[T], envs: Option<HashMap<String, String>>,
  ) -> Result<(bool, String, String)> {
      let output = Command::new(cmd).args(args.iter()).envs(envs.unwrap_or_default()).output()?;
      // returns (success, stdout, stderr)
  }
  ```
  Already used by `run_loader_command` for the `use_stdout=true` branch. Tool IPC could migrate to this, eliminating the temp file entirely. However, tool scripts currently use stdout for **user-visible output** (terminal display), meaning switching to stdout IPC would require tools to migrate user output to stderr — a **breaking tool contract change**.

### 8. `run_loader_command` has BOTH IPC modes — tempfile and stdout
- `src/utils/command.rs:103-145`: loader commands use stdout by default (`use_stdout = true`); only when the command contains `$2` placeholder does it write to a tempfile. **Neither mode deletes the output file** when `use_stdout=false`. Same leak pattern.

### 9. jsonic parse only in normal (non-nested) path
- `src/function.rs:344-356`: when `llm_output_defined = false` (normal case), JSON content detection (`starts_with('{') || starts_with('[')`) triggers jsonic parse. When `llm_output_defined = true` (nested), output is read raw with NO jsonic parse. Asymmetric behavior.

## Findings

1. **NO race condition** (CONFIRMED, `src/utils/command.rs:86`): `.status()` blocks until subprocess exits, after which aichat reads the file. POSIX file-close-on-exit guarantees data visibility. No fsync needed on local/tmpfs. On NFS mounts this could theoretically be wrong, but aichat only targets local operations.

2. **LLM_OUTPUT tempfiles are NEVER deleted** (CONFIRMED, `rg "remove_file" src/`): Neither `-eval-` files (tool call IPC) nor `-output-` files (document loader) have any cleanup code. In a long agent session with 50 tool calls, this leaves 50 orphaned files in `/tmp`. On Linux where `/tmp` is tmpfs (memory-backed), this consumes RAM until reboot. On macOS (APFS `/private/var/folders`), it consumes persistent disk space.

3. **Double `getenv("LLM_OUTPUT")` syscall per tool call** (CONFIRMED, `src/function.rs:298,331`): Two separate `env::var()` calls when one would do. Minor optimization but zero-risk fix.

4. **`envs.clone()` wastes a HashMap copy** (CONFIRMED, `src/function.rs:319`): The HashMap is built, filled with PATH (potentially hundreds of bytes), LLM_OUTPUT, and any agent variables, then cloned unnecessarily before the `run_command` call. Should be a move.

5. **Nested LLM_OUTPUT behavior reads inherited parent path** (CONFIRMED, `src/function.rs:297`): When aichat is invoked from within a tool script (which itself received LLM_OUTPUT in its env), the nested aichat sees the parent's `LLM_OUTPUT` in its process environment. It skips temp file creation and reads from the PARENT's LLM_OUTPUT file after running. This is architecturally intentional (enables `aichat --eval` as a tool that reports back into the outer call's IPC channel). However, the logic is fragile: any external shell session with `LLM_OUTPUT` set (e.g., user debugging) will trigger this branch unexpectedly.

6. **IPC alternatives analysis** (CONFIRMED via code read + THEORY for alternatives):
   - **stdout capture** (`run_command_with_output`): eliminates disk I/O, but requires tool contract change (user output → stderr). Breaking change.
   - **Named pipe (FIFO)**: same semantics as a regular file, same syscall count, more complexity. Not worth it.
   - **tmpfs on Linux**: current approach already avoids real disk I/O on Linux (where `/tmp` is tmpfs). The "file I/O" is purely memory operations. On macOS, real APFS writes occur.
   - **`cmd_args` stdin channel**: tools could read JSON arguments from stdin instead of as a command-line arg. This has no impact on the output IPC mechanism.

7. **Crypto-quality UUID for file names** (LIKELY over-engineering, `src/utils/mod.rs:197`): `uuid::Uuid::new_v4()` draws from the OS CSPRNG (getrandom syscall). For temp file naming where collision probability is already negligible with a simple counter, this is overkill. However, the absolute cost is ~microseconds and not a meaningful hotspot vs subprocess spawn.

8. **Jsonic parse asymmetry** (CONFIRMED, `src/function.rs:344-356`): Normal tool call path parses JSON output with jsonic (redundant, as confirmed by Task 5). Nested aichat path skips jsonic entirely. Both end up calling `serde_json::from_str` in `ToolCall::eval`. The jsonic parse in the normal path provides no correctness benefit (zero-copy `as_str()` returns verbatim bytes per Task 5 findings).

## Optimization Candidates

| # | Proposal | Impact | Effort | Risk | Evidence Basis |
|---|----------|--------|--------|------|----------------|
| 1 | **Add temp file cleanup**: call `fs::remove_file(temp_file)` after `fs::read_to_string` in `run_llm_function`. Wrap in `let _ =` to ignore cleanup errors. Similarly for `run_loader_command` `-output-` files. | med (resource leak fix) | S | low | CONFIRMED — zero cleanup code in codebase |
| 2 | **Merge double `env::var("LLM_OUTPUT")`**: replace two `env::var()` calls (lines 298, 331) with one `let llm_output_env = std::env::var("LLM_OUTPUT").ok()` at the top, use `llm_output_env.is_some()` and `llm_output_env.as_deref()` downstream. | low | XS | low | CONFIRMED — two separate getenv calls confirmed |
| 3 | **Move `envs` instead of clone**: change `run_command(&cmd_name, &cmd_args, Some(envs.clone()))` at function.rs:319 to `Some(envs)` (move). Saves cloning a HashMap with ~3-5 entries per tool call. | low | XS | low | CONFIRMED — envs never used after line 319 |
| 4 | **Migrate to stdout IPC (long-term)**: switch from tempfile to `run_command_with_output` for structured JSON return value, require tools to emit structured output on stdout and user display on stderr. Eliminates temp file creation, UUID generation, file read, and cleanup. | med (eliminates ~3 syscalls per tool call) | L | high (breaking tool contract) | CONFIRMED via command.rs:89 |
| 5 | **Replace UUID with sequential counter for temp file names**: use `AtomicU64` counter instead of `uuid::Uuid::new_v4()` to avoid CSPRNG syscall per temp file. File names remain collision-free within a PID. | low | XS | low | THEORY — UUID is technically over-engineered here, but the cost is ~1µs vs 1-100ms subprocess |
| 6 | **Add LLM_OUTPUT guard for external shell env pollution**: check that `LLM_OUTPUT` points to a file with expected `aichat-<PID>-` prefix before using it in the nested path. Prevents accidental behavior change when user's shell has LLM_OUTPUT set. | med (correctness/safety) | S | low | CONFIRMED risk via function.rs:298 process-env check |
| 7 | **Remove jsonic from normal output path**: since jsonic `as_str()` is zero-copy verbatim bytes (Task 5), the jsonic parse in function.rs:345-352 provides no benefit. Replace with: `if serde_json::from_str::<Value>(&contents).is_ok() { output = Some(contents); }` — or just unconditionally assign contents. | low | XS | low | CONFIRMED (Task 5 findings + function.rs:344-356) |

## Open Questions / Needs Verification

- **On macOS: does `/tmp` → APFS involve actual disk writes?** If so, the temp file approach has real I/O cost on macOS that tmpfs on Linux avoids. Needs runtime measurement.
- **Run_loader_command `-output-` files**: confirmed they're never cleaned up, but the frequency depends on how often document loaders with `$2` placeholder are used. Not in the hot agent tool-call path but same pattern.
- **Does the `run_command` `.envs()` call REPLACE or ADD TO the inherited environment?** From Rust docs, `.envs()` adds to the inherited env (doesn't replace it). This means: the child gets ALL of the parent's env vars PLUS the `envs` HashMap. Both `PATH` and `LLM_OUTPUT` are set in the `envs` HashMap, which overrides any inherited values for those keys.
- **What happens if the tool script exits nonzero but DID write to LLM_OUTPUT?** Current code at function.rs:321-323: `if exit_code != 0 { bail!(...) }` — the output is discarded and an error is returned. LLM_OUTPUT is never read. The temp file leaks regardless.
- **Sandbox/security implications of env inheritance**: If aichat runs in a context where `LLM_OUTPUT` is set by a malicious parent, it could be redirected to read from an attacker-controlled file. Low severity (attack requires process-level compromise), but worth noting for security-aware deployments.

## Hot-Path Classification

**Per-call** — all IPC overhead happens once per tool invocation:
- UUID generation (temp file path)
- `env::var("LLM_OUTPUT")` × 2 (double lookup)
- `envs.clone()` (HashMap copy)
- `fs::read_to_string(temp_file)` (read-back)

The largest cost in absolute terms is the subprocess spawn (100-5000× bigger). The temp file operations are ~10µs total and NOT the primary concern. However, the resource leak (no cleanup) is a correctness/reliability issue, not just a performance one.

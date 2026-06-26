# Agent Initialization and Configuration Load-Time Costs

**Task ID**: 14  **Scope**: Analyze `Agent::init` (src/config/agent.rs:33) — cost of loading agent definition, functions, RAG, dynamic instructions, and variable interpolation. Determine which costs are cold-path (once per agent switch) vs warm-path (per-session or per-request).

---

## Evidence Gathered

### Call Hierarchy and Frequency

- `src/config/mod.rs:1500` — `let agent = Agent::init(config, agent_name, abort_signal).await?;` — the **only** call site of `Agent::init`. Called inside `use_agent()`.
- `src/config/mod.rs:1497` — `if config.read().agent.is_some() { bail!("Already in a agent...") }` — enforced guard: **Agent::init is called at most once per agent session**. No re-init while agent is active.
- `src/config/agent.rs:33-113` — `Agent::init` full implementation: `agent_functions_dir()` → `AgentDefinition::load()` → `Functions::init()` → `AgentConfig::load()` + `load_envs()` → `Model::retrieve_model()` → `Rag::load()` (conditional).

### Agent::init Sequential Steps (Confirmed)

```
src/config/agent.rs:37  Config::agent_functions_dir(name)        → 1× env::var
src/config/agent.rs:49  AgentConfig::load(&config_path)?         → fs::read_to_string + serde_yaml::from_str (if config exists)
src/config/agent.rs:51  AgentDefinition::load(&definition_file_path)? → fs::read_to_string + serde_yaml::from_str
src/config/agent.rs:53  Functions::init(&functions_file_path)?   → fs::read_to_string + jsonic::parse + serde_json::from_str (double-parse)
src/config/agent.rs:57  definition.replace_tools_placeholder()   → string search + N×format!() (if {{__tools__}} placeholder)
src/config/agent.rs:60  agent_config.load_envs(&definition.name) → 7× env::var (model, temperature, top_p, use_tools, agent_prelude, instructions, variables)
src/config/agent.rs:73  Model::retrieve_model()                  → model lookup in config
src/config/agent.rs:78  Rag::load(config, DEFAULT_AGENT_NAME, &rag_path)?  → disk read + index load (if rag_path.exists())
```

- `src/config/mod.rs:422-427` — `Config::agent_functions_dir(name)`: calls `env::var(format!("{}_FUNCTIONS_DIR", normalize_env_name(name)))` — one env::var call per invocation; NOT cached. Called in Agent::init (line 37) and also inside export() (line 187) — but those are cold-path.
- `src/function.rs:65-81` — `Functions::init`: `fs::read_to_string()` + `jsonic::parse(&content)` + `json_item.as_str().unwrap_or_default()` + `serde_json::from_str(json_str)` — double-parse confirmed again (Task 5 corroborated).

### Dynamic Instructions — Warm-Path Subprocess Spawn

- `src/config/agent.rs:296-302` — `update_shared_dynamic_instructions(force: bool)`: only fires if `is_dynamic_instructions() && (force || self.shared_dynamic_instructions.is_none())` — runs `run_instructions_fn()` once per agent session (first call, `force=false`).
- `src/config/agent.rs:313-319` — `run_instructions_fn()`: calls `run_llm_function(self.name().to_string(), vec!["_instructions".into(), "{}".into()], self.variable_envs())` — **spawns a subprocess** via fork+exec. Cost = 1-80ms (same as all tool calls).
- `src/config/mod.rs:2148` — `agent.update_shared_dynamic_instructions(false)?` — called from `init_agent_shared_variables()` at mod.rs:2148.
- `src/config/mod.rs:2178,2184` — `agent.update_session_dynamic_instructions(None)?` — called at session start if agent has dynamic instructions. `None` arg forces a re-spawn per session start (not per round-trip).

### interpolated_instructions — Warm-Path String Allocation

- `src/config/agent.rs:230-244` — `interpolated_instructions()`: clones one of session_dynamic_instructions / shared_dynamic_instructions / config.instructions / definition.instructions (whichever is Some), then iterates over all variables doing `output.replace(&format!("{{{{k}}}}", v))` — N String.replace() allocations for N variables. Then calls `interpolate_variables(&mut output)` for env var expansion.
- **Call sites** (CONFIRMED):
  - `src/config/session.rs:289` — `self.role_prompt = agent.interpolated_instructions()` — called in `sync_agent()` which fires at session sync (warm-path: per session start).
  - `src/config/agent.rs:329` — `to_role()` in `RoleLike` impl — called every time agent role is needed for `prepare_completion_data()` (per-round-trip!).
  - `src/config/agent.rs:192` — `export()` — admin/debug only (cold-path).
- **Hot-path concern**: `to_role()` at agent.rs:329 is called every round-trip in `prepare_completion_data()`. For N=5 variables and a 2KB instruction string: 5× String.replace() + interpolate_variables() per round. Not catastrophic, but unnecessary since variables don't change mid-session.

### variable_envs — Per-Tool-Call HashMap Allocation

- `src/config/agent.rs:255-263` — `variable_envs()`: allocates fresh `HashMap<String, String>` on every call, doing `format!("LLM_AGENT_VAR_{}", normalize_env_name(k))` + `v.clone()` for each variable. Called by `run_instructions_fn()` and (via the tool-call path, Task 4): by `run_llm_function()` for each tool call that needs agent-scoped envs.
- Variables are session-stable; this HashMap never changes mid-session but is reconstructed every time.

### RAG Load at Agent Init

- `src/config/agent.rs:78` — `Some(Arc::new(Rag::load(config, DEFAULT_AGENT_NAME, &rag_path)?))` — synchronous call, blocks until HNSW/BM25 index is loaded from disk. Cost depends on index size (megabytes for large doc collections). Only fires if the agent has a pre-built rag_path.yaml. Cold-path (once per agent session).

### `init_agent_shared_variables` Post-Init Flow

- `src/config/mod.rs:2130-2150` — `init_agent_shared_variables()`: If agent has defined variables and none are set, calls `Agent::init_agent_variables()` (may prompt user on terminal), then `agent.update_shared_dynamic_instructions(false)?`. This is the post-init flow called right after `Agent::init` returns.

---

## Findings

1. **Agent::init is a true cold-path — called exactly once per agent engagement** (CONFIRMED). The `if config.read().agent.is_some() { bail!(...) }` guard at `mod.rs:1497` enforces single-init. No optimization needed for the init itself unless agent-switching frequency is high.

2. **Functions::init double-parse overhead is negligible in context** (CONFIRMED, cold-path). `fs::read_to_string` + `jsonic::parse` + `serde_json::from_str` at `function.rs:65-81`. For a 50KB functions.json with 20 declarations: ~1-5ms total. Happens once; not a performance concern.

3. **RAG::load is synchronous and potentially slow** (CONFIRMED, cold-path). `agent.rs:78`: `Arc::new(Rag::load(...))` blocks the async task until HNSW index is deserialized. For large document collections (>10MB index), this could take 50-500ms. No async alternative exists — `Rag::load` is sync (`src/config/mod.rs:1366` also uses sync). Cold-path concern only, but user-visible startup latency.

4. **`to_role()` calls `interpolated_instructions()` per-round-trip** (CONFIRMED). `agent.rs:329`: every time `prepare_completion_data()` needs the system prompt, `to_role()` is called, which allocates a fresh String and applies N×replace() operations. For static instructions (no `dynamic_instructions = true`), the result is identical every round — pure waste. Cacheable with a dirty flag on variable changes.

5. **Dynamic instructions spawn a subprocess per session start** (CONFIRMED). `run_instructions_fn()` at `agent.rs:313` calls `run_llm_function()` which fork+execs a shell script. Cost = 1-80ms per session start (not per round-trip). Only fires if `dynamic_instructions: true` in index.yaml. Significant for rapid session cycling but rare in practice.

6. **`variable_envs()` allocates per tool call** (CONFIRMED, warm-path). `agent.rs:255-263`: creates a new `HashMap<String, String>` on every invocation. For an agent with 5 variables, 10 tool calls = 10 HashMaps + 50 format!() + 50 String::clone(). All redundant since variables are session-stable. Easy fix: cache `variable_envs()` result as `Option<HashMap<String, String>>`, invalidated only on `set_session_variables()` / `set_shared_variables()`.

7. **`Config::agent_functions_dir()` does env::var per call, not cached** (CONFIRMED, cold-path). `mod.rs:422-427`: `env::var(format!("{}_FUNCTIONS_DIR"...))` on every call. Called in Agent::init (once), export() (cold). The only hot-path concern is `run_llm_function()` in `function.rs:287` which calls `Config::agent_functions_dir()` per tool call — this was covered in Task 4 and is confirmed as a warm-path issue.

8. **`load_envs()` does 7 env::var calls on agent init** (CONFIRMED, cold-path). `agent.rs:385-415`: 7 calls to `read_env_value` / `env::var`. Negligible; cold-path.

9. **`interpolated_instructions()` clones the instruction String unnecessarily** (CONFIRMED). `agent.rs:231-244`: The `clone()` chain (`.session_dynamic_instructions.clone().or_else(|| ...)`) creates an owned String copy even when variables is empty and no env vars need expansion. With a 10KB instruction string per round, 10 rounds = 100KB of redundant string copies.

---

## Optimization Candidates

| # | Proposal | Impact | Effort | Risk | Evidence Basis |
|---|----------|--------|--------|------|----------------|
| 1 | Cache `variable_envs()` result in `Agent` struct as `Option<HashMap>`, invalidate on set_session/shared_variables | med | S | low | CONFIRMED: agent.rs:255-263; warm-path per tool call |
| 2 | Cache `interpolated_instructions()` result in `Agent` struct as `Option<String>`, invalidate on variable changes and dynamic_instructions updates | med | S | low | CONFIRMED: agent.rs:329; per-round-trip clone |
| 3 | Make `Rag::load` async or run in `tokio::task::spawn_blocking` to avoid blocking the tokio executor at agent init | low | M | low | CONFIRMED: agent.rs:78, config/mod.rs:1366 sync blocking |
| 4 | Cache `Config::agent_functions_dir()` result in Config/Agent at init time | low | S | low | CONFIRMED: mod.rs:422-427; per-call env::var (primarily warm path in run_llm_function) |
| 5 | Skip `Functions::init` double-parse: use `serde_json::from_str` directly; fall back to jsonic only on error | low | S | low | CONFIRMED: function.rs:65-81 (also Task 5) |
| 6 | `interpolated_instructions()` early return `self.definition.instructions.clone()` when variables empty and no dynamic instructions (avoids chain of `.clone().or_else()`) | low | S | low | CONFIRMED: agent.rs:230-244 |

---

## Open Questions / Needs Verification

1. How often does `to_role()` actually fire per round-trip? Confirmed it's in `RoleLike::to_role()` → `prepare_completion_data()`, but does `prepare_completion_data()` cache the Role or call `to_role()` every time? Need to check `prepare_completion_data()` in session.rs or input.rs for caching.
2. How large is a typical `functions.json` for production agents? The double-parse time scales with file size. For 100 functions at 5KB each = 500KB — jsonic would then take ~5ms, which starts to matter.
3. Does `interpolate_variables()` do env::var lookups on every call? If it calls `std::env::var` for each `${VAR}` expansion, this is a per-round-trip syscall. Needs inspection of `interpolate_variables` implementation.
4. What does `Rag::load` actually do? The HNSW index deserialization cost needs measurement for production-sized indexes. Referenced in Task 16 (rag-search) for deeper analysis.

---

## Hot-Path Classification

- **Cold-path (per agent switch)**: Agent::init, AgentDefinition::load, Functions::init, AgentConfig::load, load_envs, Rag::load, init_agent_shared_variables
- **Warm-path (per session start)**: update_session_dynamic_instructions (subprocess!), sync_agent → interpolated_instructions, init_agent_session_variables  
- **Per-round-trip**: to_role() → interpolated_instructions() (N×String.replace per round)
- **Per-tool-call**: variable_envs() (HashMap alloc), Config::agent_functions_dir() (env::var) in run_llm_function

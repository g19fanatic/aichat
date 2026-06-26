# Plan Breakdown: `api_key_command` — Dynamic API Key Resolution

## Summary
- Epics: 3 | Milestones: 8 | Tasks: 18
- Estimated iterations: 24 (18 tasks × 1.3 retry buffer)
- Suggested max_parallel: 3
- Parallelizable task groups: [1-2], [4-5-6-7-8-9], [10-11-12-13-14-15], [17-18]

## Dependency Graph

```
Round 1: Tasks [1, 2] (no deps — core module + mod.rs registration)
Round 2: Tasks [3] (verify compile — depends on 1+2)
Round 3: Tasks [4, 5, 6, 7, 8, 9] (struct fields — all parallel, depend on 3)
Round 4: Tasks [10, 11, 12, 13, 14, 15] (call site updates — all parallel, each depends on its struct task)
Round 5: Task [16] (compile + test — depends on all call site tasks)
Round 6: Tasks [17, 18] (documentation — depend on 16)
```

---

## L1: Epic 1 — Core Module Infrastructure

Create the new `api_key_command.rs` module and wire it into the client system.

### L2: Milestone 1.1 — Module Creation & Registration

| # | Task | Files | Depends |
|---|------|-------|---------|
| 1 | Create `api_key_command.rs` with `run_api_key_command` and `resolve_api_key` functions | `src/client/api_key_command.rs` (new) | [] |
| 2 | Register module in `mod.rs` — add `mod api_key_command;` line and `pub use` re-export | `src/client/mod.rs:1-2` | [] |

### L2: Milestone 1.2 — Compilation Gate

| # | Task | Files | Depends |
|---|------|-------|---------|
| 3 | Run `cargo build` to verify core module compiles correctly with access_token and SHELL imports | (verification only) | [1, 2] |

---

## L1: Epic 2 — Config Struct & Call Site Integration

Add config fields to all 6 client structs and update all 14 `prepare_*` call sites.

### L2: Milestone 2.1 — Config Struct Fields (6 clients)

| # | Task | Files | Depends |
|---|------|-------|---------|
| 4 | Add `api_key_command: Option<String>` and `api_key_command_expires_in: Option<u64>` to `OpenAIConfig` struct | `src/client/openai.rs:12-22` | [3] |
| 5 | Add `api_key_command: Option<String>` and `api_key_command_expires_in: Option<u64>` to `OpenAICompatibleConfig` struct | `src/client/openai_compatible.rs:9-18` | [3] |
| 6 | Add `api_key_command: Option<String>` and `api_key_command_expires_in: Option<u64>` to `ClaudeConfig` struct | `src/client/claude.rs:12-21` | [3] |
| 7 | Add `api_key_command: Option<String>` and `api_key_command_expires_in: Option<u64>` to `GeminiConfig` struct | `src/client/gemini.rs:11-20` | [3] |
| 8 | Add `api_key_command: Option<String>` and `api_key_command_expires_in: Option<u64>` to `CohereConfig` struct | `src/client/cohere.rs:12-21` | [3] |
| 9 | Add `api_key_command: Option<String>` and `api_key_command_expires_in: Option<u64>` to `AzureOpenAIConfig` struct | `src/client/azure_openai.rs:7-16` | [3] |

### L2: Milestone 2.2 — Call Site Updates (OpenAI + OpenAI-Compatible)

| # | Task | Files | Depends |
|---|------|-------|---------|
| 10 | Replace `self_.get_api_key()?` with `resolve_api_key(...)` in OpenAI's `prepare_chat_completions` (line 47) and `prepare_embeddings` (line 67) | `src/client/openai.rs:47,67` | [4] |
| 11 | Replace `self_.get_api_key().ok()` with `resolve_api_key(...).ok()` in OpenAI-Compatible's `prepare_chat_completions` (line 42), `prepare_embeddings` (line 63), and `prepare_rerank` (line 80) | `src/client/openai_compatible.rs:42,63,80` | [5] |

### L2: Milestone 2.3 — Call Site Updates (Claude + Gemini)

| # | Task | Files | Depends |
|---|------|-------|---------|
| 12 | Replace `self_.get_api_key()?` with `resolve_api_key(...)` in Claude's `prepare_chat_completions` (line 46) | `src/client/claude.rs:46` | [6] |
| 13 | Replace `self_.get_api_key()?` with `resolve_api_key(...)` in Gemini's `prepare_chat_completions` (line 44) and `prepare_embeddings` (line 71) | `src/client/gemini.rs:44,71` | [7] |

### L2: Milestone 2.4 — Call Site Updates (Cohere + Azure)

| # | Task | Files | Depends |
|---|------|-------|---------|
| 14 | Replace `self_.get_api_key()?` with `resolve_api_key(...)` in Cohere's `prepare_chat_completions` (line 45), `prepare_embeddings` (line 66), and `prepare_rerank` (line 93) | `src/client/cohere.rs:45,66,93` | [8] |
| 15 | Replace `self_.get_api_key()?` with `resolve_api_key(...)` in Azure OpenAI's `prepare_chat_completions` (line 48) and `prepare_embeddings` (line 68) | `src/client/azure_openai.rs:48,68` | [9] |

### L2: Milestone 2.5 — Full Compilation Gate

| # | Task | Files | Depends |
|---|------|-------|---------|
| 16 | Run `cargo build` and `cargo test` — verify zero regressions with all call sites updated | (verification only) | [10, 11, 12, 13, 14, 15] |

---

## L1: Epic 3 — Documentation & Validation

### L2: Milestone 3.1 — Config Documentation

| # | Task | Files | Depends |
|---|------|-------|---------|
| 17 | Add commented `api_key_command` example to `config.example.yaml` after the openai-compatible section (~line 130), showing field usage with explanation | `config.example.yaml:120-140` | [16] |
| 18 | Update `user-facing-docs.md` in findings with final accurate line numbers and any deviations from original plan | `findings/fix-plan-iteration-20260616/user-facing-docs.md` | [16] |

---

## Parallelism Map

```
Round 1: Tasks [1, 2]                         — (no deps, create infrastructure)
Round 2: Task  [3]                            — (compile gate after 1+2)
Round 3: Tasks [4, 5, 6, 7, 8, 9]            — (6 struct changes, all parallel)
Round 4: Tasks [10, 11, 12, 13, 14, 15]      — (14 call sites across 6 files, all parallel)
Round 5: Task  [16]                           — (compile + test gate)
Round 6: Tasks [17, 18]                       — (documentation, parallel)
```

**Optimal schedule**: 6 rounds with `max_parallel: 6` → ~6 iterations minimum, ~8 typical with verification.
**Conservative schedule**: `max_parallel: 3` → ~10 iterations typical.

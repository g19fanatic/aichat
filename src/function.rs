use crate::{
    config::{Agent, Config, GlobalConfig, RetryConfig},
    utils::*,
};

use jsonic;
use rayon::prelude::*;
use anyhow::{anyhow, bail, Context, Result};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    fs,
    time::Duration,
    sync::OnceLock,
    path::{Path, PathBuf},
};

#[cfg(windows)]
const PATH_SEP: &str = ";";
#[cfg(not(windows))]
const PATH_SEP: &str = ":";

static FUNCTIONS_PATH_CACHE: OnceLock<(PathBuf, String)> = OnceLock::new();

pub fn eval_tool_calls(config: &GlobalConfig, mut calls: Vec<ToolCall>) -> Result<Vec<ToolResult>> {
    if calls.is_empty() {
        return Ok(vec![]);
    }
    calls = ToolCall::dedup(calls);
    if calls.is_empty() {
        bail!("The request was aborted because an infinite loop of function calls was detected.")
    }
    let results: Result<Vec<(ToolCall, Value)>> = calls
        .into_par_iter()
        .map(|call| {
            let result = match call.eval(config) {
                Ok(val) => {
                    if val.is_null() {
                        json!("Tool returned no response.")
                    } else {
                        val
                    }
                }
                Err(err) => {
                    json!({"error": format!("{err:#}"), "tool_call": call.name.clone(), "status": "failed_after_retries"})
                }
            };
            Ok((call, result))
        })
        .collect();
    let results = results?;
    Ok(results
        .into_iter()
        .map(|(call, result)| ToolResult::new(call, result))
        .collect())
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ToolResult {
    pub call: ToolCall,
    pub output: Value,
}

impl ToolResult {
    pub fn new(call: ToolCall, output: Value) -> Self {
        Self { call, output }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Functions {
    declarations: Vec<FunctionDeclaration>,
}

impl Functions {
    pub fn init(declarations_path: &Path) -> Result<Self> {
        let declarations: Vec<FunctionDeclaration> = if declarations_path.exists() {
            let ctx = || {
                format!(
                    "Failed to load functions at {}",
                    declarations_path.display()
                )
            };
            let content = fs::read_to_string(declarations_path).with_context(ctx)?;
            // Use jsonic to handle potentially malformed JSON in declarations file
            match jsonic::parse(&content) {
                Ok(json_item) => {
                    let json_str = json_item.as_str().unwrap_or_default();
                    serde_json::from_str(json_str).with_context(ctx)?
                },
                Err(err) => {
                    bail!("Failed to parse function declarations: {}", err);
                }
            }
        } else {
            vec![]
        };

        Ok(Self { declarations })
    }

    pub fn find(&self, name: &str) -> Option<&FunctionDeclaration> {
        self.declarations.iter().find(|v| v.name == name)
    }

    pub fn contains(&self, name: &str) -> bool {
        self.declarations.iter().any(|v| v.name == name)
    }

    pub fn declarations(&self) -> &[FunctionDeclaration] {
        &self.declarations
    }

    pub fn is_empty(&self) -> bool {
        self.declarations.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionDeclaration {
    pub name: String,
    pub description: String,
    pub parameters: JsonSchema,
    #[serde(skip_serializing, default)]
    pub agent: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonSchema {
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub type_value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub properties: Option<IndexMap<String, JsonSchema>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub items: Option<Box<JsonSchema>>,
    #[serde(rename = "anyOf", skip_serializing_if = "Option::is_none")]
    pub any_of: Option<Vec<JsonSchema>>,
    #[serde(rename = "enum", skip_serializing_if = "Option::is_none")]
    pub enum_value: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required: Option<Vec<String>>,
}

impl JsonSchema {
    pub fn is_empty_properties(&self) -> bool {
        match &self.properties {
            Some(v) => v.is_empty(),
            None => true,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ToolCall {
    pub name: String,
    pub arguments: Value,
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra_content: Option<Value>,
}

type CallConfig = (String, String, Vec<String>, HashMap<String, String>);

impl ToolCall {
    pub fn dedup(calls: Vec<Self>) -> Vec<Self> {
        let mut new_calls = vec![];
        let mut seen_ids = HashSet::new();
        let mut seen_signatures = HashSet::new();

        for call in calls.into_iter().rev() {
            let sig = format!("{}:{}", call.name, call.arguments);
            if let Some(id) = &call.id {
                if !seen_ids.contains(id) {
                    seen_ids.insert(id.clone());
                    if seen_signatures.insert(sig) {
                        new_calls.push(call);
                    }
                }
            } else {
                if seen_signatures.insert(sig) {
                    new_calls.push(call);
                }
            }
        }

        new_calls.reverse();
        new_calls
    }

    pub fn new(name: String, arguments: Value, id: Option<String>) -> Self {
        Self {
            name,
            arguments,
            id,
            extra_content: None,
        }
    }

    pub fn with_extra_content(mut self, extra_content: Option<Value>) -> Self {
        self.extra_content = extra_content;
        self
    }

    pub fn eval(&self, config: &GlobalConfig) -> Result<Value> {
        let (call_name, cmd_name, mut cmd_args, envs) = match &config.read().agent {
            Some(agent) => self.extract_call_config_from_agent(config, agent)?,
            None => self.extract_call_config_from_config(config)?,
        };

        let json_data = if self.arguments.is_object() {
            self.arguments.clone()
        } else if let Some(arguments) = self.arguments.as_str() {
            // Try serde_json first (fast path for valid JSON), fall back to jsonic
            let arguments: Value = match serde_json::from_str(arguments) {
                Ok(value) => value,
                Err(_) => {
                    // Fall back to jsonic for fuzzy/non-standard JSON
                    match jsonic::parse(arguments) {
                        Ok(json_item) => {
                            let json_str = json_item.as_str().unwrap_or_default();
                            serde_json::from_str(json_str).map_err(|err| {
                                anyhow!(
                                    "The call '{call_name}' has invalid arguments: {arguments}. Error parsing valid JSON structure: {err}"
                                )
                            })?
                        },
                        Err(err) => {
                            bail!("The call '{call_name}' has malformed JSON arguments: {arguments}. Error: {err}")
                        }
                    }
                }
            };
            arguments
        } else {
            bail!(
                "The call '{call_name}' has invalid arguments: {}",
                self.arguments
            );
        };

        cmd_args.push(json_data.to_string());

        // Resolve retry config: agent-level > global-level > hardcoded default
        let retry_config = {
            let cfg = config.read();
            cfg.agent
                .as_ref()
                .and_then(|a| a.retry_config().cloned())
                .or_else(|| cfg.tool_call_retry.clone())
                .unwrap_or_else(RetryConfig::new_default)
        };

        let output = match run_llm_function_with_retry(cmd_name, cmd_args, envs, &retry_config, &call_name)? {
            Some(contents) => serde_json::from_str(&contents)
                .ok()
                .unwrap_or_else(|| json!({"output": contents})),
            None => Value::Null,
        };

        Ok(output)
    }

    fn extract_call_config_from_agent(
        &self,
        config: &GlobalConfig,
        agent: &Agent,
    ) -> Result<CallConfig> {
        let function_name = self.name.clone();
        match agent.functions().find(&function_name) {
            Some(function) => {
                let agent_name = agent.name().to_string();
                if function.agent {
                    Ok((
                        format!("{agent_name}-{function_name}"),
                        agent_name,
                        vec![function_name],
                        agent.variable_envs(),
                    ))
                } else {
                    Ok((
                        function_name.clone(),
                        function_name,
                        vec![],
                        Default::default(),
                    ))
                }
            }
            None => self.extract_call_config_from_config(config),
        }
    }

    fn extract_call_config_from_config(&self, config: &GlobalConfig) -> Result<CallConfig> {
        let function_name = self.name.clone();
        match config.read().functions.contains(&function_name) {
            true => Ok((
                function_name.clone(),
                function_name,
                vec![],
                Default::default(),
            )),
            false => bail!("Unexpected call: {function_name} {}", self.arguments),
        }
    }
}

pub fn run_llm_function_with_retry(
    cmd_name: String,
    cmd_args: Vec<String>,
    envs: HashMap<String, String>,
    retry_config: &RetryConfig,
    call_name: &str,
) -> Result<Option<String>> {
    let max = retry_config.max_attempts.max(1);
    let mut last_err = None;

    for attempt in 0..max {
        match run_llm_function(cmd_name.clone(), cmd_args.clone(), envs.clone()) {
            Ok(output) => return Ok(output),
            Err(err) => {
                let err_msg = format!("{err:#}");
                last_err = Some(err);

                if attempt + 1 < max {
                    let delay_ms = (retry_config.delay_ms as f64
                        * retry_config.backoff_factor.powi(attempt as i32))
                        as u64;
                    eprintln!(
                        "\u{26a0}\u{fe0f}  Tool call '{}' failed (attempt {}/{}): {}. Retrying in {}ms...",
                        call_name,
                        attempt + 1,
                        max,
                        err_msg,
                        delay_ms
                    );
                    std::thread::sleep(Duration::from_millis(delay_ms));
                } else {
                    eprintln!(
                        "\u{274c} Tool call '{}' failed after {} attempts: {}",
                        call_name, max, err_msg
                    );
                }
            }
        }
    }

    Err(last_err.unwrap())
}

pub fn run_llm_function(
    cmd_name: String,
    cmd_args: Vec<String>,
    mut envs: HashMap<String, String>,
) -> Result<Option<String>> {
    debug!("run_llm_function called with cmd_name: {}", cmd_name);
    let llm_output = std::env::var("LLM_OUTPUT").ok();
    debug!("LLM_OUTPUT environment variable: {:?}", llm_output);

    let mut bin_dirs: Vec<PathBuf> = vec![];
    if cmd_args.len() > 1 {
        let dir = Config::agent_functions_dir(&cmd_name).join("bin");
        if dir.exists() {
            bin_dirs.push(dir);
        }
    }
    let (cached_bin_dir, cached_path) = FUNCTIONS_PATH_CACHE.get_or_init(|| {
        (Config::functions_bin_dir(), std::env::var("PATH").unwrap_or_default())
    });
    bin_dirs.push(cached_bin_dir.clone());
    let current_path = cached_path;
    let prepend_path = bin_dirs
        .iter()
        .map(|v| format!("{}{PATH_SEP}", v.display()))
        .collect::<Vec<_>>()
        .join("");
    envs.insert("PATH".into(), format!("{prepend_path}{current_path}"));

    // Track whether LLM_OUTPUT was predefined (used for println guard only)
    let llm_output_defined = llm_output.is_some();

    // ALWAYS create a per-call temp file for LLM_OUTPUT to prevent shared-file
    // race condition when parallel tool calls run via rayon's par_iter().
    // Even when LLM_OUTPUT is pre-set (e.g., by vim-llm-assistant), each child
    // process must write to its own isolated file to avoid result accumulation
    // across parallel calls and across multi-round tool call sequences.
    let temp_file = temp_file("-eval-", "");
    debug!("Creating per-call temporary file for LLM_OUTPUT: {}", temp_file.display());
    envs.insert("LLM_OUTPUT".into(), temp_file.display().to_string());

    #[cfg(windows)]
    let cmd_name = polyfill_cmd_name(&cmd_name, &bin_dirs);
    
    // Print if stdout is a terminal OR LLM_OUTPUT is defined
    if *IS_STDOUT_TERMINAL || llm_output_defined {
        let prompt = format!("Call {cmd_name} {}", cmd_args.join(" "));
        println!("**~~ {} ~~**", dimmed_text(&prompt));
        debug!("Displaying tool call prompt (IS_STDOUT_TERMINAL: {}, llm_output_defined: {})", *IS_STDOUT_TERMINAL, llm_output_defined);
    }

    let exit_code = run_command(&cmd_name, &cmd_args, Some(envs))
        .map_err(|err| {
            let _ = fs::remove_file(&temp_file);
            anyhow!("Unable to run {cmd_name}, {err}")
        })?;
    if exit_code != 0 {
        let _ = fs::remove_file(&temp_file);
        bail!("Tool call exit with {exit_code}");
    }

    let mut output = None;

    if temp_file.exists() {
        debug!("Reading tool output from per-call temporary file: {}", temp_file.display());
        let contents =
            fs::read_to_string(&temp_file).context("Failed to retrieve tool call output")?;
        if !contents.is_empty() {
            output = Some(contents);
        }
        let _ = fs::remove_file(&temp_file);
    }
    debug!("Tool output: {}", output.is_some());
    
    Ok(output)
}

#[cfg(windows)]
fn polyfill_cmd_name<T: AsRef<Path>>(cmd_name: &str, bin_dir: &[T]) -> String {
    let cmd_name = cmd_name.to_string();
    if let Ok(exts) = std::env::var("PATHEXT") {
        for name in exts.split(';').map(|ext| format!("{cmd_name}{ext}")) {
            for dir in bin_dir {
                let path = dir.as_ref().join(&name);
                if path.exists() {
                    return name.to_string();
                }
            }
        }
    }
    cmd_name
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn perf_q06_single_env_lookup() {
        // Verify run_llm_function still works after merging three
        // env::var("LLM_OUTPUT") calls into one cached lookup.
        let envs = HashMap::new();
        let result = run_llm_function("nonexistent_cmd_q06_test".into(), vec!["{}".into()], envs);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("nonexistent_cmd_q06_test"), "Expected cmd name in error, got: {err}");
    }

    #[test]
    fn perf_q04_envs_not_cloned() {
        // Verify run_llm_function still works after replacing envs.clone()
        // with a move at the run_command call site.
        let mut envs = HashMap::new();
        envs.insert("CUSTOM_Q04_VAR".into(), "test_value".into());
        let result = run_llm_function("nonexistent_cmd_q04_test".into(), vec!["{}".into()], envs);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("nonexistent_cmd_q04_test"), "Expected cmd name in error, got: {err}");
    }

    #[test]
    fn perf_q05_temp_file_cleanup() {
        // Verify the cleanup pattern: temp_file creates a file path,
        // writing + reading + removing leaves no leftover file.
        let path = temp_file("-eval-q05-test-", "");
        // Write content to the temp file
        fs::write(&path, "test cleanup content").expect("Failed to write temp file");
        assert!(path.exists(), "Temp file should exist after write");
        // Read it back (borrowing, not consuming)
        let contents = fs::read_to_string(&path).expect("Failed to read temp file");
        assert_eq!(contents, "test cleanup content");
        // Clean up — this is the pattern Q5 adds to production code
        let _ = fs::remove_file(&path);
        assert!(!path.exists(), "Temp file should be removed after cleanup");
    }

    #[test]
    fn perf_q12_format_gated() {
        // Behavior-preservation: run_llm_function still works after moving
        // the format!(prompt) inside the IS_STDOUT_TERMINAL guard.
        // The prompt is only used for display; moving it inside the guard
        // avoids a String allocation when output is suppressed.
        let envs = HashMap::new();
        let result = run_llm_function("nonexistent_cmd_q12_test".into(), vec!["{}".into()], envs);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("nonexistent_cmd_q12_test"), "Expected cmd name in error, got: {err}");
    }

    #[test]
    fn perf_q07_json_parse_order() {
        // Verify serde_json fast path produces identical results to jsonic pipeline
        // for well-formed JSON (the common case Q7 optimizes).
        let valid_obj = r#"{"key": "value", "num": 42}"#;
        let valid_arr = r#"[1, 2, 3]"#;

        // Fast path: serde_json parses valid JSON directly
        let fast_obj: Value = serde_json::from_str(valid_obj).unwrap();
        let fast_arr: Value = serde_json::from_str(valid_arr).unwrap();
        assert_eq!(fast_obj["key"], "value");
        assert_eq!(fast_obj["num"], 42);
        assert_eq!(fast_arr[0], 1);

        // Slow path: jsonic -> serde_json pipeline (existing behavior)
        let jsonic_obj = jsonic::parse(valid_obj).unwrap();
        let slow_obj: Value = serde_json::from_str(jsonic_obj.as_str().unwrap_or_default()).unwrap();
        let jsonic_arr = jsonic::parse(valid_arr).unwrap();
        let slow_arr: Value = serde_json::from_str(jsonic_arr.as_str().unwrap_or_default()).unwrap();

        // Both paths must produce identical results
        assert_eq!(fast_obj, slow_obj, "serde_json fast path must match jsonic pipeline for objects");
        assert_eq!(fast_arr, slow_arr, "serde_json fast path must match jsonic pipeline for arrays");
    }

    #[test]
    fn perf_q03_cached_path_consistent() {
        // Verify OnceLock-cached PATH and functions_bin_dir return
        // consistent values across multiple calls (init runs only once).
        let (dir1, path1) = FUNCTIONS_PATH_CACHE.get_or_init(|| {
            (Config::functions_bin_dir(), std::env::var("PATH").unwrap_or_default())
        });
        let (dir2, path2) = FUNCTIONS_PATH_CACHE.get_or_init(|| {
            // This closure should NEVER execute (already initialized)
            panic!("OnceLock should already be initialized")
        });
        assert_eq!(dir1, dir2, "functions_bin_dir should be consistent across calls");
        assert_eq!(path1, path2, "PATH should be consistent across calls");
        // Pointer equality proves same cached reference
        assert!(std::ptr::eq(dir1, dir2), "Should return same reference (cached)");
    }

    #[test]
    fn perf_q01_eval_preserves_order() {
        // Verify rayon par_iter preserves input ordering — the core
        // invariant that Q1 parallel eval_tool_calls relies on.
        use rayon::prelude::*;
        let inputs: Vec<u32> = (0..100).collect();
        let results: Vec<u32> = inputs.into_par_iter().map(|x| x).collect();
        assert_eq!(results, (0..100).collect::<Vec<u32>>());

        // Also verify ToolCall::dedup preserves relative order
        let calls: Vec<ToolCall> = (0..5)
            .map(|i| ToolCall::new(format!("func_{i}"), json!({}), Some(format!("id_{i}"))))
            .collect();
        let deduped = ToolCall::dedup(calls);
        for (i, call) in deduped.iter().enumerate() {
            assert_eq!(call.name, format!("func_{i}"), "dedup must preserve order at index {i}");
        }
    }
}

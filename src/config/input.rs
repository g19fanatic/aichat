use super::*;

use crate::client::{
    init_client, patch_messages, ChatCompletionsData, Client, ImageUrl, Message, MessageContent,
    MessageContentPart, MessageContentToolCalls, MessageRole, Model,
};
use crate::function::ToolResult;
use crate::utils::{base64_encode, is_loader_protocol, sha256, AbortSignal};

use anyhow::{bail, Context, Result};
use indexmap::IndexSet;
use serde_json::Value;
use std::{collections::HashMap, fs::File, io::Read};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const IMAGE_EXTS: [&str; 5] = ["png", "jpeg", "jpg", "webp", "gif"];
const SUMMARY_MAX_WIDTH: usize = 80;

/// Maximum characters per history turn field (user or assistant).
/// Truncates to prevent blowing out the context window with pathologically long turns.
const MAX_HISTORY_TURN_CHARS: usize = 16000;
// StreamingLLM (arXiv:2309.17453): initial tokens are "attention sinks" that anchor
// model coherence. Lost in the Middle (arXiv:2307.03172): models attend most to
// beginning and end positions. Split: 25% head (attention sink) + 75% tail (recency).
const HEAD_CHARS: usize = 4000;
const TAIL_CHARS: usize = 12000;

/// A parsed history turn from vim-llm-assistant's llm_history_turns JSON array.
/// Each turn represents one user/assistant exchange from the conversation history.
#[derive(Debug, Clone)]
pub struct VimHistoryTurn {
    pub user: String,
    pub assistant: String,
}

/// A content block from the split user message, annotated with cache metadata.
/// When `_cache_hints` is present in the JSON input, the user message text is split
/// into separate content blocks at field boundaries. Each block can independently
/// receive `cache_control` in the Claude API request.
#[derive(Debug, Clone)]
pub struct CacheContentBlock {
    /// The field name this block corresponds to (e.g., "buffers", "active_buffer")
    #[allow(dead_code)] // Used in tests for parsing validation
    pub(crate) field_name: String,
    /// The text content of this block
    pub text: String,
    /// Whether a cache breakpoint should be placed after this block
    pub is_breakpoint: bool,
}

#[derive(Debug, Clone)]
pub struct Input {
    config: GlobalConfig,
    text: String,
    raw: (String, Vec<String>),
    patched_text: Option<String>,
    last_reply: Option<String>,
    continue_output: Option<String>,
    regenerate: bool,
    medias: Vec<String>,
    data_urls: HashMap<String, String>,
    tool_calls: Option<MessageContentToolCalls>,
    role: Role,
    rag_name: Option<String>,
    vim_history_turns: Vec<VimHistoryTurn>,
    cache_content_blocks: Vec<CacheContentBlock>,
    cache_warm: bool,
    with_session: bool,
    with_agent: bool,
}

impl Input {
    pub fn from_str(config: &GlobalConfig, text: &str, role: Option<Role>) -> Self {
        let (role, with_session, with_agent) = resolve_role(&config.read(), role);
        Self {
            config: config.clone(),
            text: text.to_string(),
            raw: (text.to_string(), vec![]),
            patched_text: None,
            last_reply: None,
            continue_output: None,
            regenerate: false,
            medias: Default::default(),
            data_urls: Default::default(),
            tool_calls: None,
            role,
            rag_name: None,
            vim_history_turns: vec![],
            cache_content_blocks: vec![],
            cache_warm: false,
            with_session,
            with_agent,
        }
    }

    pub async fn from_files(
        config: &GlobalConfig,
        raw_text: &str,
        paths: Vec<String>,
        role: Option<Role>,
    ) -> Result<Self> {
        let loaders = config.read().document_loaders.clone();
        let (raw_paths, local_paths, remote_urls, external_cmds, protocol_paths, with_last_reply) =
            resolve_paths(&loaders, paths)?;
        let mut last_reply = None;
        let (documents, medias, data_urls) = load_documents(
            &loaders,
            local_paths,
            remote_urls,
            external_cmds,
            protocol_paths,
        )
        .await
        .context("Failed to load files")?;
        let mut texts = vec![];
        // Documents first (static content — cache-friendly prefix)
        let documents_is_empty = documents.is_empty();
        let documents_len = documents.len();
        // Try to parse vim history turns from loaded documents (JSON input from vim-llm-assistant)
        let vim_history_turns = documents.iter()
            .flat_map(|(_, _, contents)| parse_vim_history_turns(contents))
            .collect::<Vec<_>>();
        // Try to parse cache hints and split content into blocks for cache-aware message building
        let cache_content_blocks = documents.iter()
            .flat_map(|(_, _, contents)| split_json_content_blocks(contents))
            .collect::<Vec<_>>();
        // Detect cache warm flag from _cache_warm field in JSON input or AICHAT_CACHE_WARM env var
        let cache_warm = documents.iter()
            .any(|(_, _, contents)| detect_cache_warm_field(contents))
            || std::env::var("AICHAT_CACHE_WARM").ok().as_deref() == Some("1");
        for (kind, path, contents) in documents {
            if documents_len == 1 && raw_text.is_empty() {
                texts.push(format!("\n{contents}"));
            } else {
                texts.push(format!(
                    "\n{contents}\n============ {kind}: {path} ============"
                ));
            }
        }
        // Last reply (semi-static — stable per session turn)
        if with_last_reply {
            if let Some(LastMessage { input, output, .. }) = config.read().last_message.as_ref() {
                if !output.is_empty() {
                    last_reply = Some(output.clone())
                } else if let Some(v) = input.last_reply.as_ref() {
                    last_reply = Some(v.clone());
                }
                if let Some(v) = last_reply.clone() {
                    texts.push(format!("\n{v}"));
                }
            }
            if last_reply.is_none() && documents_is_empty && medias.is_empty() {
                bail!("No last reply found");
            }
        }
        // Prompt text last (dynamic content — changes every request)
        if !raw_text.is_empty() {
            texts.push(raw_text.to_string());
        }
        // Append cursor position from env vars (volatile — placed AFTER all cached content)
        // These are set by vim-llm-assistant adapter to avoid invalidating the JSON file cache
        if let (Ok(cursor_line), Ok(cursor_col)) = (
            std::env::var("AICHAT_CURSOR_LINE"),
            std::env::var("AICHAT_CURSOR_COL"),
        ) {
            if !cursor_line.is_empty() && !cursor_col.is_empty() {
                texts.push(format!("\ncursor_line:{}, cursor_col:{}", cursor_line, cursor_col));
            }
        }
        let (role, with_session, with_agent) = resolve_role(&config.read(), role);
        Ok(Self {
            config: config.clone(),
            text: texts.join("\n"),
            raw: (raw_text.to_string(), raw_paths),
            patched_text: None,
            last_reply,
            continue_output: None,
            regenerate: false,
            medias,
            data_urls,
            tool_calls: Default::default(),
            role,
            rag_name: None,
            vim_history_turns,
            cache_content_blocks,
            cache_warm,
            with_session,
            with_agent,
        })
    }

    pub async fn from_files_with_spinner(
        config: &GlobalConfig,
        raw_text: &str,
        paths: Vec<String>,
        role: Option<Role>,
        abort_signal: AbortSignal,
    ) -> Result<Self> {
        abortable_run_with_spinner(
            Input::from_files(config, raw_text, paths, role),
            "Loading files",
            abort_signal,
        )
        .await
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty() && self.medias.is_empty()
    }

    pub fn data_urls(&self) -> HashMap<String, String> {
        self.data_urls.clone()
    }

    pub fn tool_calls(&self) -> &Option<MessageContentToolCalls> {
        &self.tool_calls
    }

    pub fn text(&self) -> String {
        match self.patched_text.clone() {
            Some(text) => text,
            None => self.text.clone(),
        }
    }

    pub fn clear_patch(&mut self) {
        self.patched_text = None;
    }

    pub fn set_text(&mut self, text: String) {
        self.text = text;
    }

    pub fn stream(&self) -> bool {
        self.config.read().stream && !self.role().model().no_stream()
    }

    pub fn continue_output(&self) -> Option<&str> {
        self.continue_output.as_deref()
    }

    pub fn set_continue_output(&mut self, output: &str) {
        let output = match &self.continue_output {
            Some(v) => format!("{v}{output}"),
            None => output.to_string(),
        };
        self.continue_output = Some(output);
    }

    pub fn regenerate(&self) -> bool {
        self.regenerate
    }

    pub fn set_regenerate(&mut self) {
        let role = self.config.read().extract_role();
        if role.name() == self.role().name() {
            self.role = role;
        }
        self.regenerate = true;
        self.tool_calls = None;
    }

    pub async fn use_embeddings(&mut self, abort_signal: AbortSignal) -> Result<()> {
        if self.text.is_empty() {
            return Ok(());
        }
        let rag = self.config.read().rag.clone();
        if let Some(rag) = rag {
            let result = Config::search_rag(&self.config, &rag, &self.text, abort_signal).await?;
            self.patched_text = Some(result);
            self.rag_name = Some(rag.name().to_string());
        }
        Ok(())
    }

    pub fn rag_name(&self) -> Option<&str> {
        self.rag_name.as_deref()
    }

    pub fn merge_tool_results(mut self, output: String, tool_results: Vec<ToolResult>) -> Self {
        match self.tool_calls.as_mut() {
            Some(exist_tool_results) => {
                exist_tool_results.merge(tool_results, output);
            }
            None => self.tool_calls = Some(MessageContentToolCalls::new(tool_results, output)),
        }
        // Clear stale cache blocks: they are only meaningful for the first API call.
        // On recursive tool-result submissions, they would overwrite tool_result messages.
        self.cache_content_blocks = vec![];
        self
    }

    pub fn create_client(&self) -> Result<Box<dyn Client>> {
        init_client(&self.config, Some(self.role().model().clone()))
    }

    pub async fn fetch_chat_text(&self) -> Result<String> {
        let client = self.create_client()?;
        let text = client.chat_completions(self.clone()).await?.text;
        let text = strip_think_tag(&text).to_string();
        Ok(text)
    }

    pub fn prepare_completion_data(
        &self,
        model: &Model,
        stream: bool,
    ) -> Result<ChatCompletionsData> {
        let mut messages = self.build_messages()?;
        patch_messages(&mut messages, model);
        model.guard_max_input_tokens(&messages)?;
        let (temperature, top_p) = (self.role().temperature(), self.role().top_p());
        let functions = self.config.read().select_functions(self.role());
        Ok(ChatCompletionsData {
            messages,
            temperature,
            top_p,
            functions,
            stream,
            cache_content_blocks: self.cache_content_blocks.clone(),
            cache_warm: self.cache_warm,
        })
    }

    pub fn build_messages(&self) -> Result<Vec<Message>> {
        let mut messages = if let Some(session) = self.session(&self.config.read().session) {
            session.build_messages(self)
        } else {
            self.role().build_messages(self)
        };
        // When vim history turns are present, prepend them as proper multi-turn
        // user/assistant message pairs BEFORE the current user context message.
        // This enables Anthropic's prompt caching to work at maximum efficiency:
        // each turn is cached independently, so subsequent requests only pay for new content.
        if !self.vim_history_turns.is_empty() {
            if let Some(insert_pos) = messages.iter().rposition(|m| m.role == MessageRole::User) {
                let mut history_messages = Vec::new();
                for turn in &self.vim_history_turns {
                    if !turn.user.is_empty() {
                        history_messages.push(Message::new(
                            MessageRole::User,
                            MessageContent::Text(turn.user.clone()),
                        ));
                    }
                    if !turn.assistant.is_empty() {
                        history_messages.push(Message::new(
                            MessageRole::Assistant,
                            MessageContent::Text(turn.assistant.clone()),
                        ));
                    }
                }
                // Splice history turns before the last user message (current context)
                messages.splice(insert_pos..insert_pos, history_messages);
            }
        }
        if let Some(tool_calls) = &self.tool_calls {
            messages.push(Message::new(
                MessageRole::Assistant,
                MessageContent::ToolCalls(tool_calls.clone()),
            ))
        }
        Ok(messages)
    }

    pub fn echo_messages(&self) -> String {
        if let Some(session) = self.session(&self.config.read().session) {
            session.echo_messages(self)
        } else {
            self.role().echo_messages(self)
        }
    }

    pub fn role(&self) -> &Role {
        &self.role
    }

    pub fn session<'a>(&self, session: &'a Option<Session>) -> Option<&'a Session> {
        if self.with_session {
            session.as_ref()
        } else {
            None
        }
    }

    pub fn session_mut<'a>(&self, session: &'a mut Option<Session>) -> Option<&'a mut Session> {
        if self.with_session {
            session.as_mut()
        } else {
            None
        }
    }

    pub fn with_agent(&self) -> bool {
        self.with_agent
    }

    pub fn summary(&self) -> String {
        let text: String = self
            .text
            .trim()
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect();
        if text.width_cjk() > SUMMARY_MAX_WIDTH {
            let mut sum_width = 0;
            let mut chars = vec![];
            for c in text.chars() {
                sum_width += c.width_cjk().unwrap_or(1);
                if sum_width > SUMMARY_MAX_WIDTH - 3 {
                    chars.extend(['.', '.', '.']);
                    break;
                }
                chars.push(c);
            }
            chars.into_iter().collect()
        } else {
            text
        }
    }

    pub fn raw(&self) -> String {
        let (text, files) = &self.raw;
        let mut segments = files.to_vec();
        if !segments.is_empty() {
            segments.insert(0, ".file".into());
        }
        if !text.is_empty() {
            if !segments.is_empty() {
                segments.push("--".into());
            }
            segments.push(text.clone());
        }
        segments.join(" ")
    }

    pub fn render(&self) -> String {
        let text = self.text();
        if self.medias.is_empty() {
            return text;
        }
        let tail_text = if text.is_empty() {
            String::new()
        } else {
            format!(" -- {text}")
        };
        let files: Vec<String> = self
            .medias
            .iter()
            .cloned()
            .map(|url| resolve_data_url(&self.data_urls, url))
            .collect();
        format!(".file {}{}", files.join(" "), tail_text)
    }

    pub fn message_content(&self) -> MessageContent {
        if self.medias.is_empty() {
            MessageContent::Text(self.text())
        } else {
            let mut list: Vec<MessageContentPart> = self
                .medias
                .iter()
                .cloned()
                .map(|url| MessageContentPart::ImageUrl {
                    image_url: ImageUrl { url },
                })
                .collect();
            if !self.text.is_empty() {
                list.insert(0, MessageContentPart::Text { text: self.text() });
            }
            MessageContent::Array(list)
        }
    }
}

/// Parse vim-llm-assistant's `llm_history_turns` array from JSON content.
///
/// Expects JSON with a top-level `llm_history_turns` array field, where each element
/// has `user` (required) and `assistant` (optional) string fields.
/// Returns empty Vec if the content is not JSON, has no `llm_history_turns` field,
/// or if the field is empty/malformed.
///
/// Edge case handling:
/// - Empty/whitespace-only user text: turn is skipped (no meaningful prompt)
/// - Missing/null assistant: treated as empty string (turn in progress)
/// - Extremely long content (>16000 chars): truncated with "(…truncated)" marker
/// - Special characters (unicode, newlines, etc.): passed through unchanged
/// - Non-string fields: turn is skipped
/// - Numeric/boolean user/assistant values: turn is skipped (as_str returns None)
/// - Empty turns array: returns empty Vec
pub fn parse_vim_history_turns(content: &str) -> Vec<VimHistoryTurn> {
    // Quick prefix check to avoid parsing non-JSON content
    let trimmed = content.trim_start();
    if !trimmed.starts_with('{') {
        return vec![];
    }

    let json: Value = match serde_json::from_str(content) {
        Ok(v) => v,
        Err(_) => return vec![],
    };

    let turns_array = match json.get("llm_history_turns").and_then(|v| v.as_array()) {
        Some(arr) => arr,
        None => return vec![],
    };

    let mut turns = Vec::new();
    for turn in turns_array {
        // user field is required — skip turns without it
        let user_raw = match turn.get("user").and_then(|v| v.as_str()) {
            Some(s) if !s.trim().is_empty() => s,
            None => continue,
            Some(_) => continue, // empty/whitespace-only user — skip
        };
        // assistant field is optional — use empty string if missing (turn in progress)
        let assistant_raw = turn.get("assistant")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        // Truncate overly long content to prevent context window exhaustion
        let user = truncate_turn_content(user_raw);
        let assistant = truncate_turn_content(assistant_raw);

        turns.push(VimHistoryTurn { user, assistant });
    }
    turns
}

/// Truncate a turn's content if it exceeds MAX_HISTORY_TURN_CHARS.
///
/// Uses a head+tail strategy: keeps the first HEAD_CHARS bytes (attention sink) and
/// the last TAIL_CHARS bytes (recency bias), replacing the middle with a sentinel.
/// Both cut points are walked to safe UTF-8 char boundaries.
fn truncate_turn_content(content: &str) -> String {
    if content.len() <= MAX_HISTORY_TURN_CHARS {
        return content.to_string();
    }

    // Find safe head end (walk backwards to char boundary)
    let mut head_end = HEAD_CHARS;
    while head_end > 0 && !content.is_char_boundary(head_end) {
        head_end -= 1;
    }

    // Find safe tail start (walk forwards to char boundary)
    let tail_target = content.len().saturating_sub(TAIL_CHARS);
    let mut tail_start = tail_target;
    while tail_start < content.len() && !content.is_char_boundary(tail_start) {
        tail_start += 1;
    }

    let dropped = tail_start - head_end;
    format!(
        "{}\n[... {} chars truncated from middle ...]\n{}",
        &content[..head_end],
        dropped,
        &content[tail_start..]
    )
}

/// Parse `_cache_hints` from JSON content and split field values into content blocks.
///
/// Reads the top-level JSON object, extracts the `_cache_hints` field to determine
/// which fields are breakpoints, then iterates over the remaining content fields
/// in their JSON order to produce annotated content blocks.
///
/// Fields that are metadata (prefixed with `_`) or handled separately (like
/// `llm_history_turns`) are excluded from the output blocks.
///
/// Returns empty Vec if:
/// - Content is not JSON
/// - JSON has no `_cache_hints` field
/// - `_cache_hints.breakpoint_after` is missing or empty
pub fn split_json_content_blocks(content: &str) -> Vec<CacheContentBlock> {
    // Quick prefix check to avoid parsing non-JSON content
    let trimmed = content.trim_start();
    if !trimmed.starts_with('{') {
        return vec![];
    }

    let json: Value = match serde_json::from_str(content) {
        Ok(v) => v,
        Err(_) => return vec![],
    };

    let obj = match json.as_object() {
        Some(o) => o,
        None => return vec![],
    };

    // Extract _cache_hints — if absent, no splitting is performed
    let hints = match obj.get("_cache_hints") {
        Some(h) => h,
        None => return vec![],
    };

    let breakpoint_after: Vec<String> = hints
        .get("breakpoint_after")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    if breakpoint_after.is_empty() {
        return vec![];
    }

    // Fields to skip — metadata or handled separately
    let skip_fields = ["_cache_hints", "llm_history_turns"];

    let mut blocks = Vec::new();

    // Iterate over JSON object keys in their original order (serde_json Map preserves insertion order)
    for (key, value) in obj.iter() {
        if skip_fields.contains(&key.as_str()) || key.starts_with('_') {
            continue;
        }

        // Format the field value as text content
        let text = match value {
            Value::String(s) => s.clone(),
            Value::Null => continue, // skip null fields
            _ => {
                // For arrays/objects, serialize to compact JSON with a label
                format!("{}:{}", key, value)
            }
        };

        if text.is_empty() {
            continue;
        }

        let is_breakpoint = breakpoint_after.contains(key);

        blocks.push(CacheContentBlock {
            field_name: key.clone(),
            text,
            is_breakpoint,
        });
    }

    blocks
}

/// Detect `_cache_warm` field in JSON content.
///
/// Returns true if the JSON input has a truthy `_cache_warm` field.
/// Accepts: `true`, `1`, `"1"`, `"true"` as truthy values.
/// Returns false for non-JSON content or missing/falsy `_cache_warm` field.
pub fn detect_cache_warm_field(content: &str) -> bool {
    let trimmed = content.trim_start();
    if !trimmed.starts_with('{') {
        return false;
    }

    let json: Value = match serde_json::from_str(content) {
        Ok(v) => v,
        Err(_) => return false,
    };

    match json.get("_cache_warm") {
        Some(Value::Bool(true)) => true,
        Some(Value::Number(n)) => n.as_u64() == Some(1),
        Some(Value::String(s)) => s == "1" || s == "true",
        _ => false,
    }
}

fn resolve_role(config: &Config, role: Option<Role>) -> (Role, bool, bool) {
    match role {
        Some(v) => (v, false, false),
        None => (
            config.extract_role(),
            config.session.is_some(),
            config.agent.is_some(),
        ),
    }
}

type ResolvePathsOutput = (
    Vec<String>,
    Vec<String>,
    Vec<String>,
    Vec<String>,
    Vec<String>,
    bool,
);

fn resolve_paths(
    loaders: &HashMap<String, String>,
    paths: Vec<String>,
) -> Result<ResolvePathsOutput> {
    let mut raw_paths = IndexSet::new();
    let mut local_paths = IndexSet::new();
    let mut remote_urls = IndexSet::new();
    let mut external_cmds = IndexSet::new();
    let mut protocol_paths = IndexSet::new();
    let mut with_last_reply = false;
    for path in paths {
        if path == "%%" {
            with_last_reply = true;
            raw_paths.insert(path);
        } else if path.starts_with('`') && path.len() > 2 && path.ends_with('`') {
            external_cmds.insert(path[1..path.len() - 1].to_string());
            raw_paths.insert(path);
        } else if is_url(&path) {
            if path.strip_suffix("**").is_some() {
                bail!("Invalid website '{path}'");
            }
            remote_urls.insert(path.clone());
            raw_paths.insert(path);
        } else if is_loader_protocol(loaders, &path) {
            protocol_paths.insert(path.clone());
            raw_paths.insert(path);
        } else {
            let resolved_path = resolve_home_dir(&path);
            let absolute_path = to_absolute_path(&resolved_path)
                .with_context(|| format!("Invalid path '{path}'"))?;
            local_paths.insert(resolved_path);
            raw_paths.insert(absolute_path);
        }
    }
    Ok((
        raw_paths.into_iter().collect(),
        local_paths.into_iter().collect(),
        remote_urls.into_iter().collect(),
        external_cmds.into_iter().collect(),
        protocol_paths.into_iter().collect(),
        with_last_reply,
    ))
}

async fn load_documents(
    loaders: &HashMap<String, String>,
    local_paths: Vec<String>,
    remote_urls: Vec<String>,
    external_cmds: Vec<String>,
    protocol_paths: Vec<String>,
) -> Result<(
    Vec<(&'static str, String, String)>,
    Vec<String>,
    HashMap<String, String>,
)> {
    let mut files = vec![];
    let mut medias = vec![];
    let mut data_urls = HashMap::new();

    for cmd in external_cmds {
        let output = duct::cmd(&SHELL.cmd, &[&SHELL.arg, &cmd])
            .stderr_to_stdout()
            .unchecked()
            .read()
            .unwrap_or_else(|err| err.to_string());
        files.push(("CMD", cmd, output));
    }

    let local_files = expand_glob_paths(&local_paths, true).await?;
    for file_path in local_files {
        if is_image(&file_path) {
            let contents = read_media_to_data_url(&file_path)
                .with_context(|| format!("Unable to read media '{file_path}'"))?;
            data_urls.insert(sha256(&contents), file_path);
            medias.push(contents)
        } else {
            let document = load_file(loaders, &file_path)
                .await
                .with_context(|| format!("Unable to read file '{file_path}'"))?;
            files.push(("FILE", file_path, document.contents));
        }
    }

    for file_url in remote_urls {
        let (contents, extension) = fetch_with_loaders(loaders, &file_url, true)
            .await
            .with_context(|| format!("Failed to load url '{file_url}'"))?;
        if extension == MEDIA_URL_EXTENSION {
            data_urls.insert(sha256(&contents), file_url);
            medias.push(contents)
        } else {
            files.push(("URL", file_url, contents));
        }
    }

    for protocol_path in protocol_paths {
        let documents = load_protocol_path(loaders, &protocol_path)
            .with_context(|| format!("Failed to load from '{protocol_path}'"))?;
        files.extend(
            documents
                .into_iter()
                .map(|document| ("FROM", document.path, document.contents)),
        );
    }

    Ok((files, medias, data_urls))
}

pub fn resolve_data_url(data_urls: &HashMap<String, String>, data_url: String) -> String {
    if data_url.starts_with("data:") {
        let hash = sha256(&data_url);
        if let Some(path) = data_urls.get(&hash) {
            return path.to_string();
        }
        data_url
    } else {
        data_url
    }
}

fn is_image(path: &str) -> bool {
    get_patch_extension(path)
        .map(|v| IMAGE_EXTS.contains(&v.as_str()))
        .unwrap_or_default()
}

fn read_media_to_data_url(image_path: &str) -> Result<String> {
    let extension = get_patch_extension(image_path).unwrap_or_default();
    let mime_type = match extension.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        _ => bail!("Unexpected media type"),
    };
    let mut file = File::open(image_path)?;
    let mut buffer = Vec::new();
    file.read_to_end(&mut buffer)?;

    let encoded_image = base64_encode(buffer);
    let data_url = format!("data:{mime_type};base64,{encoded_image}");

    Ok(data_url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_vim_history_turns_normal() {
        let json = r#"{"llm_history_turns": [
            {"user": "Hello", "assistant": "Hi there!"},
            {"user": "How are you?", "assistant": "I'm doing well."}
        ]}"#;
        let turns = parse_vim_history_turns(json);
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].user, "Hello");
        assert_eq!(turns[0].assistant, "Hi there!");
        assert_eq!(turns[1].user, "How are you?");
        assert_eq!(turns[1].assistant, "I'm doing well.");
    }

    #[test]
    fn test_parse_vim_history_turns_empty_assistant() {
        // Turn in progress — assistant hasn't responded yet
        let json = r#"{"llm_history_turns": [
            {"user": "Hello", "assistant": ""},
            {"user": "Still waiting"}
        ]}"#;
        let turns = parse_vim_history_turns(json);
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].user, "Hello");
        assert_eq!(turns[0].assistant, "");
        assert_eq!(turns[1].user, "Still waiting");
        assert_eq!(turns[1].assistant, "");
    }

    #[test]
    fn test_parse_vim_history_turns_null_assistant() {
        let json = r#"{"llm_history_turns": [
            {"user": "Hello", "assistant": null}
        ]}"#;
        let turns = parse_vim_history_turns(json);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].user, "Hello");
        assert_eq!(turns[0].assistant, "");
    }

    #[test]
    fn test_parse_vim_history_turns_missing_user() {
        // Turns without user field should be skipped entirely
        let json = r#"{"llm_history_turns": [
            {"assistant": "orphan response"},
            {"user": "Valid turn", "assistant": "Valid response"}
        ]}"#;
        let turns = parse_vim_history_turns(json);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].user, "Valid turn");
    }

    #[test]
    fn test_parse_vim_history_turns_empty_user() {
        // Empty/whitespace user should be skipped
        let json = r#"{"llm_history_turns": [
            {"user": "", "assistant": "response to nothing"},
            {"user": "   ", "assistant": "response to whitespace"},
            {"user": "\t\n", "assistant": "response to control chars"},
            {"user": "Valid", "assistant": "OK"}
        ]}"#;
        let turns = parse_vim_history_turns(json);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].user, "Valid");
        assert_eq!(turns[0].assistant, "OK");
    }

    #[test]
    fn test_parse_vim_history_turns_special_characters() {
        let json = r#"{"llm_history_turns": [
            {"user": "Hello 🌍 世界", "assistant": "こんにちは! 🎉"},
            {"user": "Line1\nLine2\tTabbed", "assistant": "\"Quoted\" & <escaped>"},
            {"user": "Back\\slash", "assistant": "Forward/slash"}
        ]}"#;
        let turns = parse_vim_history_turns(json);
        assert_eq!(turns.len(), 3);
        assert_eq!(turns[0].user, "Hello 🌍 世界");
        assert_eq!(turns[0].assistant, "こんにちは! 🎉");
        assert_eq!(turns[1].user, "Line1\nLine2\tTabbed");
        assert_eq!(turns[1].assistant, "\"Quoted\" & <escaped>");
        assert_eq!(turns[2].user, "Back\\slash");
        assert_eq!(turns[2].assistant, "Forward/slash");
    }

    #[test]
    fn test_parse_vim_history_turns_non_json() {
        assert_eq!(parse_vim_history_turns("not json at all").len(), 0);
        assert_eq!(parse_vim_history_turns("").len(), 0);
        assert_eq!(parse_vim_history_turns("[1,2,3]").len(), 0);
        assert_eq!(parse_vim_history_turns("plain text content").len(), 0);
    }

    #[test]
    fn test_parse_vim_history_turns_no_field() {
        let json = r#"{"other_field": "value", "prompt": "hello"}"#;
        assert_eq!(parse_vim_history_turns(json).len(), 0);
    }

    #[test]
    fn test_parse_vim_history_turns_empty_array() {
        let json = r#"{"llm_history_turns": []}"#;
        assert_eq!(parse_vim_history_turns(json).len(), 0);
    }

    #[test]
    fn test_parse_vim_history_turns_non_string_values() {
        // Numeric/boolean values for user/assistant should be handled gracefully
        let json = r#"{"llm_history_turns": [
            {"user": 42, "assistant": "response"},
            {"user": true, "assistant": "response"},
            {"user": "Valid", "assistant": 123}
        ]}"#;
        let turns = parse_vim_history_turns(json);
        // First two turns skipped (user not a string), third has empty assistant (not a string)
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].user, "Valid");
        assert_eq!(turns[0].assistant, "");
    }

    #[test]
    fn test_parse_vim_history_turns_truncation() {
        // Create content exceeding MAX_HISTORY_TURN_CHARS
        let long_user = "x".repeat(MAX_HISTORY_TURN_CHARS + 100);
        let long_assistant = "y".repeat(MAX_HISTORY_TURN_CHARS + 500);
        let json = format!(
            r#"{{"llm_history_turns": [{{"user": "{}", "assistant": "{}"}}]}}"#,
            long_user, long_assistant
        );
        let turns = parse_vim_history_turns(&json);
        assert_eq!(turns.len(), 1);
        // User should be truncated
        assert!(turns[0].user.len() < long_user.len());
        // Head preserved (first HEAD_CHARS chars)
        assert!(turns[0].user.starts_with(&"x".repeat(HEAD_CHARS)));
        // Tail preserved (last TAIL_CHARS chars)
        assert!(turns[0].user.ends_with(&"x".repeat(TAIL_CHARS)));
        // Sentinel present in middle
        assert!(turns[0].user.contains("[..."));
        // Assistant should be truncated
        assert!(turns[0].assistant.len() < long_assistant.len());
        assert!(turns[0].assistant.starts_with(&"y".repeat(HEAD_CHARS)));
        assert!(turns[0].assistant.ends_with(&"y".repeat(TAIL_CHARS)));
        assert!(turns[0].assistant.contains("[..."));
    }

    #[test]
    fn test_truncate_turn_content_under_limit() {
        let short = "Hello, world!";
        assert_eq!(truncate_turn_content(short), short);
    }

    #[test]
    fn test_truncate_turn_content_at_limit() {
        let exact = "a".repeat(MAX_HISTORY_TURN_CHARS);
        assert_eq!(truncate_turn_content(&exact), exact);
    }

    #[test]
    fn test_truncate_turn_content_over_limit() {
        let over = "b".repeat(MAX_HISTORY_TURN_CHARS + 1000);
        let result = truncate_turn_content(&over);
        // Head preserved (first HEAD_CHARS)
        assert!(result.starts_with(&"b".repeat(HEAD_CHARS)));
        // Tail preserved (last TAIL_CHARS)
        assert!(result.ends_with(&"b".repeat(TAIL_CHARS)));
        // Sentinel present in middle
        assert!(result.contains("[..."));
        // Overall length is less than original
        assert!(result.len() < over.len());
    }

    #[test]
    fn test_truncate_turn_content_multibyte_boundary() {
        // '─' is U+2500, 3 bytes in UTF-8 (E2 94 80)

        // Test 1: multibyte char at HEAD cut point
        // Place '─' so byte HEAD_CHARS falls inside it (head_end walks back to HEAD_CHARS-1)
        let head_prefix = "a".repeat(HEAD_CHARS - 1); // 3999 ASCII bytes
        let padding = "b".repeat(MAX_HISTORY_TURN_CHARS + 100); // enough to exceed limit
        let input1 = format!("{}─{}", head_prefix, padding);
        // Must not panic (UTF-8 boundary safety)
        let result1 = truncate_turn_content(&input1);
        // head_end walked back to HEAD_CHARS-1 (before the '─')
        assert!(result1.starts_with(&head_prefix));
        // '─' should NOT appear in the head portion
        assert!(!result1[..head_prefix.len()].contains('─'));
        // Sentinel must be present
        assert!(result1.contains("[..."));

        // Test 2: multibyte char at TAIL cut point
        // input2.len() = 4000 + 3 + 11998 = 16001, tail_target = 4001 (inside '─')
        // tail_start walks forward from 4001 to 4003 (first byte after '─')
        let tail_prefix2 = "c".repeat(4000);
        let tail_suffix2 = "d".repeat(11998);
        let input2 = format!("{}─{}", tail_prefix2, tail_suffix2);
        // input2.len() = 4000 + 3 + 11998 = 16001 > MAX_HISTORY_TURN_CHARS
        // Must not panic (UTF-8 boundary safety)
        let result2 = truncate_turn_content(&input2);
        // Sentinel present
        assert!(result2.contains("[..."));
        // Valid UTF-8 throughout (would panic if sliced at invalid boundary)
        let _char_count = result2.chars().count();
        // Tail ends with the 'd' suffix (all 'd's from after '─' onward)
        assert!(result2.ends_with(&tail_suffix2));
    }

    #[test]
    fn test_truncate_turn_content_exact_sentinel_format() {
        // Verify the EXACT sentinel format string and dropped byte count
        // All-ASCII content: head_end=HEAD_CHARS, tail_start=content.len()-TAIL_CHARS
        // dropped = tail_start - head_end (exact byte count of the dropped middle)
        let extra = 100usize;
        let content = "z".repeat(MAX_HISTORY_TURN_CHARS + extra);
        let result = truncate_turn_content(&content);

        // Exact head content
        assert!(result.starts_with(&"z".repeat(HEAD_CHARS)));
        // Exact tail content
        assert!(result.ends_with(&"z".repeat(TAIL_CHARS)));

        // Compute expected dropped count
        let head_end = HEAD_CHARS; // all ASCII
        let tail_start = (MAX_HISTORY_TURN_CHARS + extra) - TAIL_CHARS; // = HEAD_CHARS + extra = 4100
        let expected_dropped = tail_start - head_end; // = extra = 100
        let expected_sentinel = format!("\n[... {} chars truncated from middle ...]\n", expected_dropped);

        // Exact sentinel format
        assert!(
            result.contains(&expected_sentinel),
            "Expected sentinel '{}' not found in result",
            expected_sentinel
        );

        // Sentinel appears exactly once
        assert_eq!(
            result.matches(&expected_sentinel as &str).count(),
            1,
            "Sentinel should appear exactly once"
        );
    }

    #[test]
    fn test_truncate_turn_content_exact_limit_boundary() {
        // Exactly one byte over the limit → should truncate with sentinel
        // Note: result.len() may be LARGER than content.len() when the dropped middle
        // (content.len() - HEAD_CHARS - TAIL_CHARS = 1 byte) is smaller than the sentinel text.
        // The key invariants are: head preserved, tail preserved, sentinel present.
        let content = "q".repeat(MAX_HISTORY_TURN_CHARS + 1);
        let result = truncate_turn_content(&content);
        // head preserved
        assert!(result.starts_with(&"q".repeat(HEAD_CHARS)));
        // tail preserved
        assert!(result.ends_with(&"q".repeat(TAIL_CHARS)));
        // sentinel present (truncation was applied) — middle 1 byte replaced by sentinel text
        assert!(result.contains("\n[... 1 chars truncated from middle ...]\n"));
    }

    #[test]
    fn test_truncate_turn_content_no_sentinel_at_limit() {
        // Content exactly at the limit → no truncation, no sentinel
        let content = "r".repeat(MAX_HISTORY_TURN_CHARS);
        let result = truncate_turn_content(&content);
        // No sentinel
        assert!(!result.contains("[..."));
        // Identical to input
        assert_eq!(result, content);
    }

    #[test]
    fn test_split_json_content_blocks_normal() {
        let json = r#"{"llm_history": "history text", "buffers": [{"name": "test.rs"}], "active_buffer": "fn main() {}", "prompt": "explain this", "_cache_hints": {"breakpoint_after": ["llm_history", "buffers"], "stable_fields": ["llm_history", "buffers"], "dynamic_fields": ["prompt"]}}"#;
        let blocks = split_json_content_blocks(json);
        assert_eq!(blocks.len(), 4);
        // llm_history is a string field
        assert_eq!(blocks[0].field_name, "llm_history");
        assert_eq!(blocks[0].text, "history text");
        assert!(blocks[0].is_breakpoint);
        // buffers is an array — serialized with label
        assert_eq!(blocks[1].field_name, "buffers");
        assert!(blocks[1].text.starts_with("buffers:"));
        assert!(blocks[1].is_breakpoint);
        // active_buffer is a string
        assert_eq!(blocks[2].field_name, "active_buffer");
        assert_eq!(blocks[2].text, "fn main() {}");
        assert!(!blocks[2].is_breakpoint);
        // prompt is a string
        assert_eq!(blocks[3].field_name, "prompt");
        assert_eq!(blocks[3].text, "explain this");
        assert!(!blocks[3].is_breakpoint);
    }

    #[test]
    fn test_split_json_content_blocks_no_hints() {
        // Without _cache_hints, returns empty
        let json = r#"{"llm_history": "text", "buffers": [], "prompt": "hello"}"#;
        let blocks = split_json_content_blocks(json);
        assert!(blocks.is_empty());
    }

    #[test]
    fn test_split_json_content_blocks_empty_breakpoint_after() {
        // With empty breakpoint_after array, returns empty
        let json = r#"{"prompt": "hello", "_cache_hints": {"breakpoint_after": []}}"#;
        let blocks = split_json_content_blocks(json);
        assert!(blocks.is_empty());
    }

    #[test]
    fn test_split_json_content_blocks_skips_metadata_fields() {
        // Fields starting with _ and llm_history_turns are skipped
        let json = r#"{"llm_history_turns": [{"user": "hi"}], "buffers": "buf content", "_internal": "skip me", "prompt": "hello", "_cache_hints": {"breakpoint_after": ["buffers"]}}"#;
        let blocks = split_json_content_blocks(json);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].field_name, "buffers");
        assert_eq!(blocks[1].field_name, "prompt");
    }

    #[test]
    fn test_split_json_content_blocks_skips_null_and_empty() {
        let json = r#"{"field1": null, "field2": "", "field3": "content", "_cache_hints": {"breakpoint_after": ["field3"]}}"#;
        let blocks = split_json_content_blocks(json);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].field_name, "field3");
        assert_eq!(blocks[0].text, "content");
        assert!(blocks[0].is_breakpoint);
    }

    #[test]
    fn test_split_json_content_blocks_non_json() {
        assert!(split_json_content_blocks("not json").is_empty());
        assert!(split_json_content_blocks("").is_empty());
        assert!(split_json_content_blocks("[1,2,3]").is_empty());
    }

    #[test]
    fn test_split_json_content_blocks_preserves_order() {
        // Verify that field order from the JSON is preserved
        let json = r#"{"alpha": "aaa", "beta": "bbb", "gamma": "ggg", "_cache_hints": {"breakpoint_after": ["beta"]}}"#;
        let blocks = split_json_content_blocks(json);
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[0].field_name, "alpha");
        assert!(!blocks[0].is_breakpoint);
        assert_eq!(blocks[1].field_name, "beta");
        assert!(blocks[1].is_breakpoint);
        assert_eq!(blocks[2].field_name, "gamma");
        assert!(!blocks[2].is_breakpoint);
    }

    #[test]
    fn test_split_json_content_blocks_object_value() {
        // Object values get serialized with field_name: prefix
        let json = r#"{"config": {"key": "value", "num": 42}, "prompt": "hi", "_cache_hints": {"breakpoint_after": ["config"]}}"#;
        let blocks = split_json_content_blocks(json);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].field_name, "config");
        assert!(blocks[0].text.starts_with("config:"));
        assert!(blocks[0].text.contains("\"key\""));
        assert!(blocks[0].is_breakpoint);
    }
}

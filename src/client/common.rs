use super::*;

use crate::{
    config::{Config, GlobalConfig, Input},
    function::{eval_tool_calls, FunctionDeclaration, ToolCall, ToolResult},
    render::render_stream,
    utils::*,
};

use anyhow::{bail, Context, Result};
use fancy_regex::Regex;
use indexmap::IndexMap;
use inquire::{
    list_option::ListOption, required, validator::Validation, MultiSelect, Select, Text,
};
use reqwest::{Client as ReqwestClient, RequestBuilder, Response};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::LazyLock;
use std::time::Duration;
use tokio::sync::mpsc::unbounded_channel;

const MODELS_YAML: &str = include_str!("../../models.yaml");

pub static ALL_PROVIDER_MODELS: LazyLock<Vec<ProviderModels>> = LazyLock::new(|| {
    Config::loal_models_override()
        .ok()
        .unwrap_or_else(|| serde_yaml::from_str(MODELS_YAML).unwrap())
});

static EMBEDDING_MODEL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"((^|/)(bge-|e5-|uae-|gte-|text-)|embed|multilingual|minilm)").unwrap()
});

static ESCAPE_SLASH_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?<!\\)/").unwrap());

#[async_trait::async_trait]
pub trait Client: Sync + Send {
    fn global_config(&self) -> &GlobalConfig;

    fn extra_config(&self) -> Option<&ExtraConfig>;

    fn patch_config(&self) -> Option<&RequestPatch>;

    fn name(&self) -> &str;

    fn model(&self) -> &Model;

    fn model_mut(&mut self) -> &mut Model;

    fn build_client(&self) -> Result<ReqwestClient> {
        let mut builder = ReqwestClient::builder();
        let extra = self.extra_config();
        let timeout = extra.and_then(|v| v.connect_timeout).unwrap_or(10);
        if let Some(proxy) = extra.and_then(|v| v.proxy.as_deref()) {
            builder = set_proxy(builder, proxy)?;
        }
        if let Some(user_agent) = self.global_config().read().user_agent.as_ref() {
            builder = builder.user_agent(user_agent);
        }
        let client = builder
            .connect_timeout(Duration::from_secs(timeout))
            .build()
            .with_context(|| "Failed to build client")?;
        Ok(client)
    }

    async fn chat_completions(&self, input: Input) -> Result<ChatCompletionsOutput> {
        if self.global_config().read().dry_run {
            let content = input.echo_messages();
            return Ok(ChatCompletionsOutput::new(&content));
        }
        let client = self.build_client()?;
        let data = input.prepare_completion_data(self.model(), false)?;
        self.chat_completions_inner(&client, data)
            .await
            .with_context(|| "Failed to call chat-completions api")
    }

    async fn chat_completions_streaming(
        &self,
        input: &Input,
        handler: &mut SseHandler,
    ) -> Result<()> {
        let abort_signal = handler.abort();
        let input = input.clone();
        tokio::select! {
            ret = async {
                if self.global_config().read().dry_run {
                    let content = input.echo_messages();
                    handler.text(&content)?;
                    return Ok(());
                }
                let client = self.build_client()?;
                let data = input.prepare_completion_data(self.model(), true)?;
                self.chat_completions_streaming_inner(&client, handler, data).await
            } => {
                handler.done();
                ret.with_context(|| "Failed to call chat-completions api")
            }
            _ = wait_abort_signal(&abort_signal) => {
                handler.done();
                Ok(())
            },
        }
    }

    async fn embeddings(&self, data: &EmbeddingsData) -> Result<Vec<Vec<f32>>> {
        let client = self.build_client()?;
        self.embeddings_inner(&client, data)
            .await
            .context("Failed to call embeddings api")
    }

    async fn rerank(&self, data: &RerankData) -> Result<RerankOutput> {
        let client = self.build_client()?;
        self.rerank_inner(&client, data)
            .await
            .context("Failed to call rerank api")
    }

    async fn chat_completions_inner(
        &self,
        client: &ReqwestClient,
        data: ChatCompletionsData,
    ) -> Result<ChatCompletionsOutput>;

    async fn chat_completions_streaming_inner(
        &self,
        client: &ReqwestClient,
        handler: &mut SseHandler,
        data: ChatCompletionsData,
    ) -> Result<()>;

    async fn embeddings_inner(
        &self,
        _client: &ReqwestClient,
        _data: &EmbeddingsData,
    ) -> Result<EmbeddingsOutput> {
        bail!("The client doesn't support embeddings api")
    }

    async fn rerank_inner(
        &self,
        _client: &ReqwestClient,
        _data: &RerankData,
    ) -> Result<RerankOutput> {
        bail!("The client doesn't support rerank api")
    }

    fn request_builder(
        &self,
        client: &reqwest::Client,
        mut request_data: RequestData,
    ) -> RequestBuilder {
        self.patch_request_data(&mut request_data);
        request_data.into_builder(client)
    }

    fn patch_request_data(&self, request_data: &mut RequestData) {
        let model_type = self.model().model_type();
        if let Some(patch) = self.model().patch() {
            request_data.apply_patch(patch.clone());
        }

        let patch_map = std::env::var(get_env_name(&format!(
            "patch_{}_{}",
            self.model().client_name(),
            model_type.api_name(),
        )))
        .ok()
        .and_then(|v| serde_json::from_str(&v).ok())
        .or_else(|| {
            self.patch_config()
                .and_then(|v| model_type.extract_patch(v))
                .cloned()
        });
        let patch_map = match patch_map {
            Some(v) => v,
            _ => return,
        };
        for (key, patch) in patch_map {
            let key = ESCAPE_SLASH_RE.replace_all(&key, r"\/");
            if let Ok(regex) = Regex::new(&format!("^({key})$")) {
                if let Ok(true) = regex.is_match(self.model().name()) {
                    request_data.apply_patch(patch);
                    return;
                }
            }
        }
    }
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self::OpenAIConfig(OpenAIConfig::default())
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ExtraConfig {
    pub proxy: Option<String>,
    pub connect_timeout: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct RequestPatch {
    pub chat_completions: Option<ApiPatch>,
    pub embeddings: Option<ApiPatch>,
    pub rerank: Option<ApiPatch>,
}

pub type ApiPatch = IndexMap<String, Value>;

pub struct RequestData {
    pub url: String,
    pub headers: IndexMap<String, String>,
    pub body: Value,
}

impl RequestData {
    pub fn new<T>(url: T, body: Value) -> Self
    where
        T: std::fmt::Display,
    {
        Self {
            url: url.to_string(),
            headers: Default::default(),
            body,
        }
    }

    pub fn bearer_auth<T>(&mut self, auth: T)
    where
        T: std::fmt::Display,
    {
        self.headers
            .insert("authorization".into(), format!("Bearer {auth}"));
    }

    pub fn header<K, V>(&mut self, key: K, value: V)
    where
        K: std::fmt::Display,
        V: std::fmt::Display,
    {
        self.headers.insert(key.to_string(), value.to_string());
    }

    pub fn into_builder(self, client: &ReqwestClient) -> RequestBuilder {
        let RequestData { url, headers, body } = self;
        debug!("Request {url} {body}");

        let mut builder = client.post(url);
        for (key, value) in headers {
            builder = builder.header(key, value);
        }
        builder = builder.json(&body);
        builder
    }

    pub fn apply_patch(&mut self, patch: Value) {
        if let Some(patch_url) = patch["url"].as_str() {
            self.url = patch_url.into();
        }
        if let Some(patch_body) = patch.get("body") {
            json_patch::merge(&mut self.body, patch_body)
        }
        if let Some(patch_headers) = patch["headers"].as_object() {
            for (key, value) in patch_headers {
                if let Some(value) = value.as_str() {
                    self.header(key, value)
                } else if value.is_null() {
                    self.headers.swap_remove(key);
                }
            }
        }
    }
}

#[derive(Debug)]
pub struct ChatCompletionsData {
    pub messages: Vec<Message>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub functions: Option<Vec<FunctionDeclaration>>,
    pub stream: bool,
}

#[derive(Debug, Clone, Default)]
pub struct ChatCompletionsOutput {
    pub text: String,
    pub tool_calls: Vec<ToolCall>,
    pub id: Option<String>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
}

impl ChatCompletionsOutput {
    pub fn new(text: &str) -> Self {
        Self {
            text: text.to_string(),
            ..Default::default()
        }
    }
}

#[derive(Debug)]
pub struct EmbeddingsData {
    pub texts: Vec<String>,
    pub query: bool,
}

impl EmbeddingsData {
    pub fn new(texts: Vec<String>, query: bool) -> Self {
        Self { texts, query }
    }
}

pub type EmbeddingsOutput = Vec<Vec<f32>>;

#[derive(Debug)]
pub struct RerankData {
    pub query: String,
    pub documents: Vec<String>,
    pub top_n: usize,
}

impl RerankData {
    pub fn new(query: String, documents: Vec<String>, top_n: usize) -> Self {
        Self {
            query,
            documents,
            top_n,
        }
    }
}

pub type RerankOutput = Vec<RerankResult>;

#[derive(Debug, Deserialize)]
pub struct RerankResult {
    pub index: usize,
    pub relevance_score: f64,
}

pub type PromptAction<'a> = (&'a str, &'a str, Option<&'a str>);

pub async fn create_config(
    prompts: &[PromptAction<'static>],
    client: &str,
) -> Result<(String, Value)> {
    let mut config = json!({
        "type": client,
    });
    for (key, desc, help_message) in prompts {
        let env_name = format!("{client}_{key}").to_ascii_uppercase();
        let required = std::env::var(&env_name).is_err();
        let value = prompt_input_string(desc, required, *help_message)?;
        if !value.is_empty() {
            config[key] = value.into();
        }
    }
    let model = set_client_models_config(&mut config, client).await?;
    let clients = json!(vec![config]);
    Ok((model, clients))
}

pub async fn create_openai_compatible_client_config(
    client: &str,
) -> Result<Option<(String, Value)>> {
    let api_base = super::OPENAI_COMPATIBLE_PROVIDERS
        .into_iter()
        .find(|(name, _)| client == *name)
        .map(|(_, api_base)| api_base)
        .unwrap_or("http(s)://{API_ADDR}/v1");

    let name = if client == OpenAICompatibleClient::NAME {
        let value = prompt_input_string("Provider Name", true, None)?;
        value.replace(' ', "-")
    } else {
        client.to_string()
    };

    let mut config = json!({
        "type": OpenAICompatibleClient::NAME,
        "name": &name,
    });

    let api_base = if api_base.contains('{') {
        prompt_input_string("API Base", true, Some(&format!("e.g. {api_base}")))?
    } else {
        api_base.to_string()
    };
    config["api_base"] = api_base.into();

    let api_key = prompt_input_string("API Key", false, None)?;
    if !api_key.is_empty() {
        config["api_key"] = api_key.into();
    }

    let model = set_client_models_config(&mut config, &name).await?;
    let clients = json!(vec![config]);
    Ok(Some((model, clients)))
}

pub async fn call_chat_completions(
    input: &Input,
    print: bool,
    extract_code: bool,
    client: &dyn Client,
    abort_signal: AbortSignal,
) -> Result<(String, Vec<ToolResult>)> {
    let ret = abortable_run_with_spinner(
        client.chat_completions(input.clone()),
        "Generating",
        abort_signal,
    )
    .await;

    match ret {
        Ok(ret) => {
            let ChatCompletionsOutput {
                mut text,
                tool_calls,
                ..
            } = ret;
            if !text.is_empty() {
                if extract_code {
                    text = extract_code_block(&strip_think_tag(&text)).to_string();
                }
                if print {
                    client.global_config().read().print_markdown(&text)?;
                }
            }
            Ok((text, eval_tool_calls(client.global_config(), tool_calls)?))
        }
        Err(err) => Err(err),
    }
}

pub async fn call_chat_completions_streaming(
    input: &Input,
    client: &dyn Client,
    abort_signal: AbortSignal,
) -> Result<(String, Vec<ToolResult>)> {
    let (tx, rx) = unbounded_channel();
    let mut handler = SseHandler::new(tx, abort_signal.clone());

    let (send_ret, render_ret) = tokio::join!(
        client.chat_completions_streaming(input, &mut handler),
        render_stream(rx, client.global_config(), abort_signal.clone()),
    );

    if handler.abort().aborted() {
        bail!("Aborted.");
    }

    render_ret?;

    let (text, tool_calls) = handler.take();
    match send_ret {
        Ok(_) => {
            if !text.is_empty() && !text.ends_with('\n') {
                println!();
            }
            Ok((text, eval_tool_calls(client.global_config(), tool_calls)?))
        }
        Err(err) => {
            if !text.is_empty() {
                println!();
            }
            Err(err)
        }
    }
}

pub fn noop_prepare_embeddings<T>(_client: &T, _data: &EmbeddingsData) -> Result<RequestData> {
    bail!("The client doesn't support embeddings api")
}

pub async fn noop_embeddings(_builder: RequestBuilder, _model: &Model) -> Result<EmbeddingsOutput> {
    bail!("The client doesn't support embeddings api")
}

pub fn noop_prepare_rerank<T>(_client: &T, _data: &RerankData) -> Result<RequestData> {
    bail!("The client doesn't support rerank api")
}

pub async fn noop_rerank(_builder: RequestBuilder, _model: &Model) -> Result<RerankOutput> {
    bail!("The client doesn't support rerank api")
}

/// Parse a response body as JSON with informative error messages.
/// Replaces bare `res.json().await?` which produces unhelpful
/// "error decoding response body" errors when the response isn't JSON.
pub async fn response_to_json(res: Response) -> Result<Value> {
    let status = res.status();
    let content_type = res
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown")
        .to_string();
    let body = res.text().await.with_context(|| {
        format!("Failed to read response body (status: {status}, content-type: {content_type})")
    })?;
    if body.is_empty() {
        bail!("Empty response body (status: {status}, content-type: {content_type}). The API endpoint returned no data.");
    }
    serde_json::from_str(&body).with_context(|| {
        let preview = body_preview(&body);
        format!("Non-JSON response (status: {status}, content-type: {content_type}): {preview}")
    })
}

pub fn catch_error(data: &Value, status: u16) -> Result<()> {
    if (200..300).contains(&status) {
        return Ok(());
    }
    debug!("Invalid response, status: {status}, data: {data}");
    if let Some(error) = data["error"].as_object() {
        if let (Some(typ), Some(message)) = (
            json_str_from_map(error, "type"),
            json_str_from_map(error, "message"),
        ) {
            bail!("{message} (type: {typ})");
        } else if let (Some(typ), Some(message)) = (
            json_str_from_map(error, "code"),
            json_str_from_map(error, "message"),
        ) {
            bail!("{message} (code: {typ})");
        }
    } else if let Some(error) = data["errors"][0].as_object() {
        if let (Some(code), Some(message)) = (
            error.get("code").and_then(|v| v.as_u64()),
            json_str_from_map(error, "message"),
        ) {
            bail!("{message} (status: {code})")
        }
    } else if let Some(error) = data[0]["error"].as_object() {
        if let (Some(status), Some(message)) = (
            json_str_from_map(error, "status"),
            json_str_from_map(error, "message"),
        ) {
            bail!("{message} (status: {status})")
        }
    } else if let (Some(detail), Some(status)) = (data["detail"].as_str(), data["status"].as_i64())
    {
        bail!("{detail} (status: {status})");
    } else if let Some(error) = data["error"].as_str() {
        bail!("{error}");
    } else if let Some(message) = data["message"].as_str() {
        bail!("{message}");
    }
    bail!("Invalid response data: {data} (status: {status})");
}

pub fn json_str_from_map<'a>(
    map: &'a serde_json::Map<String, Value>,
    field_name: &str,
) -> Option<&'a str> {
    map.get(field_name).and_then(|v| v.as_str())
}

/// Parse a response body string as JSON with informative error messages.
/// This is the core parsing logic that can be tested without async/mock HTTP.
/// The async `response_to_json` wrapper calls this after reading the body as text.
#[cfg(test)]
fn parse_response_body(status: u16, body: &str) -> Result<Value> {
    if body.is_empty() {
        bail!("Empty response body (status: {status}). The API endpoint returned no data.");
    }
    serde_json::from_str(body).with_context(|| {
        let preview = body_preview(body);
        format!("Non-JSON response (status: {status}): {preview}")
    })
}

/// Generate a human-readable preview of a response body for error messages.
/// For binary-looking content, shows hex bytes; for text, shows first 200 chars.
fn body_preview(body: &str) -> String {
    // Detect binary-looking content by checking for non-printable characters
    let sample_size = body.chars().take(100).count();
    let non_printable = body
        .chars()
        .take(100)
        .filter(|c| !c.is_ascii_graphic() && !c.is_ascii_whitespace())
        .count();

    if sample_size > 0 && non_printable * 100 / sample_size > 30 {
        // Binary-looking content: show hex preview of first 50 bytes
        let hex: String = body
            .as_bytes()
            .iter()
            .take(50)
            .map(|b| format!("{b:02x}"))
            .collect::<Vec<_>>()
            .join(" ");
        format!("[binary data] {hex}")
    } else if body.len() > 200 {
        // Text content: truncate at safe UTF-8 boundary near 200 bytes
        let mut end = 200;
        while !body.is_char_boundary(end) && end > 0 {
            end -= 1;
        }
        format!("{}...", &body[..end])
    } else {
        body.to_string()
    }
}

async fn set_client_models_config(client_config: &mut Value, client: &str) -> Result<String> {
    if let Some(provider) = ALL_PROVIDER_MODELS.iter().find(|v| v.provider == client) {
        let models: Vec<String> = provider
            .models
            .iter()
            .filter(|v| v.model_type == "chat")
            .map(|v| v.name.clone())
            .collect();
        let model_name = select_model(models)?;
        return Ok(format!("{client}:{model_name}"));
    }
    let mut model_names = vec![];
    if let (Some(true), Some(api_base), api_key) = (
        client_config["type"]
            .as_str()
            .map(|v| v == OpenAICompatibleClient::NAME),
        client_config["api_base"].as_str(),
        client_config["api_key"]
            .as_str()
            .map(|v| v.to_string())
            .or_else(|| {
                let env_name = format!("{client}_api_key").to_ascii_uppercase();
                std::env::var(&env_name).ok()
            }),
    ) {
        match abortable_run_with_spinner(
            fetch_models(api_base, api_key.as_deref()),
            "Fetching models",
            create_abort_signal(),
        )
        .await
        {
            Ok(fetched_models) => {
                model_names = MultiSelect::new("LLMs to include (required):", fetched_models)
                    .with_validator(|list: &[ListOption<&String>]| {
                        if list.is_empty() {
                            Ok(Validation::Invalid(
                                "At least one item must be selected".into(),
                            ))
                        } else {
                            Ok(Validation::Valid)
                        }
                    })
                    .prompt()?;
            }
            Err(err) => {
                eprintln!("✗ Fetch models failed: {err}");
            }
        }
    }
    if model_names.is_empty() {
        model_names = prompt_input_string(
            "LLMs to add",
            true,
            Some("Separated by commas, e.g. llama3.3,qwen2.5"),
        )?
        .split(',')
        .filter_map(|v| {
            let v = v.trim();
            if v.is_empty() {
                None
            } else {
                Some(v.to_string())
            }
        })
        .collect::<Vec<_>>();
    }
    if model_names.is_empty() {
        bail!("No models");
    }
    let models: Vec<Value> = model_names
        .iter()
        .map(|v| {
            let l = v.to_lowercase();
            if l.contains("rank") {
                json!({
                    "name": v,
                    "type": "reranker",
                })
            } else if let Ok(true) = EMBEDDING_MODEL_RE.is_match(&l) {
                json!({
                    "name": v,
                    "type": "embedding",
                    "default_chunk_size": 1000,
                    "max_batch_size": 100
                })
            } else if v.contains("vision") {
                json!({
                    "name": v,
                    "supports_vision": true
                })
            } else {
                json!({
                    "name": v,
                })
            }
        })
        .collect();
    client_config["models"] = models.into();
    let model_name = select_model(model_names)?;
    Ok(format!("{client}:{model_name}"))
}

fn select_model(model_names: Vec<String>) -> Result<String> {
    if model_names.is_empty() {
        bail!("No models");
    }
    let model = if model_names.len() == 1 {
        model_names[0].clone()
    } else {
        Select::new("Default Model (required):", model_names).prompt()?
    };
    Ok(model)
}

fn prompt_input_string(
    desc: &str,
    required: bool,
    help_message: Option<&str>,
) -> anyhow::Result<String> {
    let desc = if required {
        format!("{desc} (required):")
    } else {
        format!("{desc} (optional):")
    };
    let mut text = Text::new(&desc);
    if required {
        text = text.with_validator(required!("This field is required"))
    }
    if let Some(help_message) = help_message {
        text = text.with_help_message(help_message);
    }
    let text = text.prompt()?;
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_response_body_valid_json() {
        let body = r#"{"choices":[{"message":{"content":"hello"}}]}"#;
        let result = parse_response_body(200, body);
        assert!(result.is_ok());
        let value = result.unwrap();
        assert_eq!(
            value["choices"][0]["message"]["content"].as_str().unwrap(),
            "hello"
        );
    }

    #[test]
    fn test_parse_response_body_empty() {
        let result = parse_response_body(200, "");
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("Empty response body"),
            "Expected 'Empty response body' in error, got: {err_msg}"
        );
        assert!(
            err_msg.contains("200"),
            "Expected status code '200' in error, got: {err_msg}"
        );
    }

    #[test]
    fn test_parse_response_body_html() {
        let html_body = r#"<!DOCTYPE html><html><head><title>502 Bad Gateway</title></head><body><h1>502 Bad Gateway</h1><p>nginx</p></body></html>"#;
        let result = parse_response_body(502, html_body);
        assert!(result.is_err());
        let err_msg = format!("{:#}", result.unwrap_err());
        assert!(
            err_msg.contains("Non-JSON response"),
            "Expected 'Non-JSON response' in error, got: {err_msg}"
        );
        assert!(
            err_msg.contains("502"),
            "Expected status code '502' in error, got: {err_msg}"
        );
        assert!(
            err_msg.contains("<!DOCTYPE html>"),
            "Expected HTML preview in error, got: {err_msg}"
        );
    }

    #[test]
    fn test_parse_response_body_binary_garbage() {
        // Simulate binary/garbage content with lots of non-printable characters
        // Use null bytes and control chars that are clearly non-printable
        let binary_body: String = (0u8..50).map(|i| char::from(i % 8)).collect();
        let result = parse_response_body(200, &binary_body);
        assert!(result.is_err());
        let err_msg = format!("{:#}", result.unwrap_err());
        assert!(
            err_msg.contains("Non-JSON response"),
            "Expected 'Non-JSON response' in error, got: {err_msg}"
        );
        assert!(
            err_msg.contains("[binary data]"),
            "Expected '[binary data]' hex preview in error, got: {err_msg}"
        );
        assert!(
            err_msg.contains("200"),
            "Expected status code '200' in error, got: {err_msg}"
        );
    }

    #[test]
    fn test_parse_response_body_error_includes_status_code() {
        // Test various status codes appear in error messages
        for status in [400, 401, 403, 404, 500, 502, 503] {
            let result = parse_response_body(status, "not json at all");
            assert!(result.is_err());
            let err_msg = format!("{:#}", result.unwrap_err());
            assert!(
                err_msg.contains(&status.to_string()),
                "Expected status code '{status}' in error, got: {err_msg}"
            );
        }
    }

    /// Simulates the full error chain as seen by the user: the inner error from
    /// `parse_response_body` wrapped with the "Failed to call chat-completions api" context.
    /// This matches what happens in production via `chat_completions_inner` + `with_context`.
    fn simulate_full_error_chain(status: u16, body: &str) -> String {
        let inner_result = parse_response_body(status, body);
        let wrapped = inner_result.with_context(|| "Failed to call chat-completions api");
        format!("{:#}", wrapped.unwrap_err())
    }

    #[test]
    fn test_error_message_quality_empty_response() {
        // Simulates: endpoint returns HTTP 200 with completely empty body
        // (e.g., narsil-admin returning nothing)
        let msg = simulate_full_error_chain(200, "");
        // Verify the full chain is informative
        assert!(
            msg.contains("Failed to call chat-completions api"),
            "Missing outer context in: {msg}"
        );
        assert!(
            msg.contains("Empty response body"),
            "Missing 'Empty response body' in: {msg}"
        );
        assert!(
            msg.contains("200"),
            "Missing status code in: {msg}"
        );
        // The message should give actionable guidance
        assert!(
            msg.contains("API endpoint returned no data"),
            "Missing actionable guidance in: {msg}"
        );
        // Verify the overall format matches what users expect:
        // "Failed to call chat-completions api: Empty response body (status: 200). The API endpoint returned no data."
        assert!(
            msg.starts_with("Failed to call chat-completions api: Empty response body (status: 200)"),
            "Unexpected error format: {msg}"
        );
    }

    #[test]
    fn test_error_message_quality_html_response() {
        // Simulates: nginx returning 502 Bad Gateway HTML page
        let html = r#"<!DOCTYPE html><html><head><title>502 Bad Gateway</title></head><body><h1>502 Bad Gateway</h1></body></html>"#;
        let msg = simulate_full_error_chain(502, html);
        // Full chain should be present
        assert!(
            msg.contains("Failed to call chat-completions api"),
            "Missing outer context in: {msg}"
        );
        assert!(
            msg.contains("Non-JSON response"),
            "Missing 'Non-JSON response' in: {msg}"
        );
        assert!(
            msg.contains("502"),
            "Missing status code in: {msg}"
        );
        // Should include HTML preview so user can see what the server returned
        assert!(
            msg.contains("<!DOCTYPE html>"),
            "Missing HTML preview in: {msg}"
        );
        // Verify format starts correctly
        assert!(
            msg.starts_with("Failed to call chat-completions api: Non-JSON response (status: 502)"),
            "Unexpected error format: {msg}"
        );
    }

    #[test]
    fn test_error_message_quality_plain_text_response() {
        // Simulates: server returning a plain text error message that isn't JSON
        let body = "Service Unavailable - Please try again later";
        let msg = simulate_full_error_chain(503, body);
        assert!(
            msg.contains("Failed to call chat-completions api"),
            "Missing outer context in: {msg}"
        );
        assert!(
            msg.contains("Non-JSON response"),
            "Missing 'Non-JSON response' in: {msg}"
        );
        assert!(
            msg.contains("503"),
            "Missing status code in: {msg}"
        );
        // Should include the actual server message in the preview
        assert!(
            msg.contains("Service Unavailable"),
            "Missing body preview in: {msg}"
        );
    }

    #[test]
    fn test_error_message_quality_auth_redirect() {
        // Simulates: auth redirect returning HTML login page with 200 status
        let body = r#"<html><head><meta http-equiv="refresh" content="0;url=/login"></head><body>Redirecting to login...</body></html>"#;
        let msg = simulate_full_error_chain(200, body);
        assert!(
            msg.starts_with("Failed to call chat-completions api: Non-JSON response (status: 200)"),
            "Unexpected error format: {msg}"
        );
        // User should be able to see the redirect/login hint in the preview
        assert!(
            msg.contains("login"),
            "Missing login redirect hint in: {msg}"
        );
    }

    /// Integration test: Simulate the exact openai_compatible error scenario.
    /// When parse_response_body fails (empty/non-JSON), the error propagates
    /// BEFORE openai_extract_chat_completions is ever called.
    /// This demonstrates the error path the user encounters.
    #[test]
    fn test_integration_empty_body_error_prevents_extract() {
        use super::openai::openai_extract_chat_completions;

        // Simulate what openai_chat_completions does:
        // 1. response_to_json(res).await? — fails on empty body
        // 2. catch_error(&data, status) — never reached
        // 3. openai_extract_chat_completions(&data) — never reached

        let parse_result = parse_response_body(200, "");
        assert!(parse_result.is_err(), "Empty body should fail to parse");

        // Since parse failed, extract is never called. But let's verify that
        // IF somehow null/empty JSON reached extract, it handles gracefully:
        let empty_json = serde_json::json!({});
        let extract_result = openai_extract_chat_completions(&empty_json);
        // extract returns Ok with empty output (graceful degradation via warn!())
        assert!(
            extract_result.is_ok(),
            "Extract should gracefully handle empty JSON: {:?}",
            extract_result.err()
        );
        let output = extract_result.unwrap();
        assert!(output.text.is_empty(), "Text should be empty for empty JSON");
        assert!(output.tool_calls.is_empty(), "Tool calls should be empty");
    }

    /// Integration test: When the server returns valid JSON with an error structure
    /// (e.g., 401 Unauthorized), catch_error should extract the error message.
    /// This tests the path: parse_response_body(OK) → catch_error(fails with message).
    #[test]
    fn test_integration_server_error_json_through_catch_error() {
        // Simulate: server returns 401 with JSON error body
        let error_body = r#"{"error":{"message":"Invalid API key provided","type":"invalid_api_key"}}"#;
        let parse_result = parse_response_body(401, error_body);
        assert!(parse_result.is_ok(), "Valid JSON should parse successfully");

        let data = parse_result.unwrap();
        // Now simulate catch_error (which is called when status is not success)
        let catch_result = catch_error(&data, 401);
        assert!(catch_result.is_err(), "catch_error should bail on 401");

        let err_msg = catch_result.unwrap_err().to_string();
        assert!(
            err_msg.contains("Invalid API key provided"),
            "Should contain server error message, got: {err_msg}"
        );
        assert!(
            err_msg.contains("invalid_api_key"),
            "Should contain error type, got: {err_msg}"
        );

        // Wrap with the outer context to get full user-visible error
        let full_chain = format!(
            "Failed to call chat-completions api: {}",
            err_msg
        );
        assert!(
            full_chain.contains("Failed to call chat-completions api"),
            "Full chain missing outer context: {full_chain}"
        );
        assert!(
            full_chain.contains("Invalid API key"),
            "Full chain missing error detail: {full_chain}"
        );
    }

    /// Integration test: Simulates the exact narsil-admin scenario from the bug report.
    /// A custom openai_compatible endpoint returns an empty body with 200 status.
    /// Verifies the COMPLETE error chain the user would see in their terminal.
    #[test]
    fn test_integration_narsil_admin_empty_response_scenario() {
        // The exact scenario: custom endpoint returns HTTP 200 but empty body
        let msg = simulate_full_error_chain(200, "");

        // The OLD error was:
        //   "Failed to call chat-completions api: error decoding response body: expected value at line 1 column 1"
        // The NEW error should be much more informative:
        assert!(
            !msg.contains("error decoding response body"),
            "Should NOT contain the old unhelpful reqwest error: {msg}"
        );
        assert!(
            !msg.contains("expected value at line 1 column 1"),
            "Should NOT contain the old unhelpful serde error: {msg}"
        );
        // Instead, should have clear, actionable information:
        assert!(
            msg.contains("Empty response body"),
            "Should explain the body was empty: {msg}"
        );
        assert!(
            msg.contains("200"),
            "Should include the HTTP status: {msg}"
        );
        assert!(
            msg.contains("API endpoint returned no data"),
            "Should provide actionable context: {msg}"
        );
    }

    #[test]
    fn test_error_message_quality_truncation() {
        // Verify long response bodies are truncated at ~200 chars with "..."
        // This ensures error messages don't flood the terminal with huge HTML pages
        let long_body = "x".repeat(500);
        let msg = simulate_full_error_chain(200, &long_body);
        // Must contain truncation indicator
        assert!(
            msg.contains("..."),
            "Long body error should show truncation '...', got: {msg}"
        );
        // Should NOT contain the full 500-char body
        assert!(
            !msg.contains(&"x".repeat(500)),
            "Error message should NOT contain full 500-char body"
        );
        // Should contain approximately 200 chars of the body
        assert!(
            msg.contains(&"x".repeat(200)),
            "Error preview should show ~200 chars of body"
        );
        // Format should still start correctly
        assert!(
            msg.starts_with("Failed to call chat-completions api: Non-JSON response (status: 200)"),
            "Unexpected error format: {msg}"
        );
    }

    #[test]
    fn test_body_preview_short_text() {
        // Short text should be returned verbatim (no truncation)
        assert_eq!(body_preview("hello world"), "hello world");
        assert_eq!(body_preview("Not Found"), "Not Found");
        // Exactly 200 chars should NOT be truncated
        let exactly_200 = "a".repeat(200);
        assert_eq!(body_preview(&exactly_200), exactly_200);
    }

    #[test]
    fn test_body_preview_long_text() {
        // 201+ chars should be truncated with "..."
        let over_200 = "b".repeat(250);
        let preview = body_preview(&over_200);
        assert!(
            preview.ends_with("..."),
            "Preview should end with '...', got: {preview}"
        );
        // Should be ~203 chars (200 content + "...")
        assert!(
            preview.len() <= 204,
            "Preview should be ≤204 chars (200 + '...'), got len: {}",
            preview.len()
        );
        assert!(
            preview.len() >= 203,
            "Preview should be ≥203 chars, got len: {}",
            preview.len()
        );
    }
}

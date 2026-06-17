use super::*;

use crate::utils::{base64_decode, encode_uri, hex_encode, hmac_sha256, sha256, strip_think_tag};

use anyhow::{bail, Context, Result};
use aws_smithy_eventstream::frame::{DecodedFrame, MessageFrameDecoder};
use aws_smithy_eventstream::smithy::parse_response_headers;
use bytes::BytesMut;
use chrono::{DateTime, Utc};
use futures_util::StreamExt;
use indexmap::IndexMap;
use reqwest::{Client as ReqwestClient, Method, RequestBuilder};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Debug, Clone, Deserialize)]
pub struct BedrockConfig {
    pub name: Option<String>,
    pub access_key_id: Option<String>,
    pub secret_access_key: Option<String>,
    pub region: Option<String>,
    pub session_token: Option<String>,
    pub profile: Option<String>,
    #[serde(default)]
    pub models: Vec<ModelData>,
    pub patch: Option<RequestPatch>,
    pub extra: Option<ExtraConfig>,
}

/// Determines which API protocol to use for a given Bedrock model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BedrockModelCategory {
    /// Default: AWS Converse API (/model/{id}/converse)
    Converse,
    /// OpenAI Chat Completions API (/v1/chat/completions)
    OpenAI,
}

impl BedrockModelCategory {
    fn from_model_name(model_name: &str) -> Self {
        if model_name.starts_with("openai.") {
            BedrockModelCategory::OpenAI
        } else {
            BedrockModelCategory::Converse
        }
    }
}

impl BedrockClient {
    config_get_fn!(access_key_id, get_access_key_id);
    config_get_fn!(secret_access_key, get_secret_access_key);
    config_get_fn!(region, get_region);
    config_get_fn!(session_token, get_session_token);

    pub const PROMPTS: [PromptAction<'static>; 3] = [
        ("access_key_id", "AWS Access Key ID", None),
        ("secret_access_key", "AWS Secret Access Key", None),
        ("region", "AWS Region", None),
    ];

    fn chat_completions_builder(
        &self,
        client: &ReqwestClient,
        data: ChatCompletionsData,
    ) -> Result<(RequestBuilder, BedrockModelCategory)> {
        let model_name = self.model.real_name();
        let model_category = BedrockModelCategory::from_model_name(&model_name);

        let config_profile = self.config.profile.as_deref();
        let (access_key_id, secret_access_key, session_token) =
            fetch_bedrock_creds_from_cli(config_profile).unwrap_or_else(|| (
                self.get_access_key_id().unwrap_or_default(),
                self.get_secret_access_key().unwrap_or_default(),
                self.get_session_token().ok(),
            ));
        let region = self.get_region()?;
        let host = match model_category {
            BedrockModelCategory::OpenAI => format!("bedrock-mantle.{region}.api.aws"),
            BedrockModelCategory::Converse => format!("bedrock-runtime.{region}.amazonaws.com"),
        };

        let (uri, body) = match model_category {
            BedrockModelCategory::Converse => {
                let uri = if data.stream {
                    format!("/model/{model_name}/converse-stream")
                } else {
                    format!("/model/{model_name}/converse")
                };
                let body = build_chat_completions_body(data, &self.model)?;
                (uri, body)
            }
            BedrockModelCategory::OpenAI => {
                let uri = "/openai/v1/responses".to_string();
                let body = build_responses_api_body(data, &self.model);
                (uri, body)
            }
        };

        let mut request_data = RequestData::new("", body);
        self.patch_request_data(&mut request_data);
        let RequestData {
            url: _,
            mut headers,
            body,
        } = request_data;

        let service = match model_category {
            BedrockModelCategory::OpenAI => "bedrock-mantle",
            BedrockModelCategory::Converse => "bedrock",
        };

        if model_category == BedrockModelCategory::OpenAI {
            headers.insert("x-amzn-mantle-client-agent".into(), "codex".into());
        }

        let builder = aws_fetch(
            client,
            &AwsCredentials {
                access_key_id,
                secret_access_key,
                region,
                session_token,
            },
            AwsRequest {
                method: Method::POST,
                host,
                service: service.into(),
                uri,
                querystring: "".into(),
                headers,
                body: body.to_string(),
            },
        )?;

        Ok((builder, model_category))
    }

    fn embeddings_builder(
        &self,
        client: &ReqwestClient,
        data: &EmbeddingsData,
    ) -> Result<RequestBuilder> {
        let config_profile = self.config.profile.as_deref();
        let (access_key_id, secret_access_key, session_token) =
            fetch_bedrock_creds_from_cli(config_profile).unwrap_or_else(|| (
                self.get_access_key_id().unwrap_or_default(),
                self.get_secret_access_key().unwrap_or_default(),
                self.get_session_token().ok(),
            ));
        let region = self.get_region()?;
        let host = format!("bedrock-runtime.{region}.amazonaws.com");

        let uri = format!("/model/{}/invoke", self.model.real_name());

        let input_type = match data.query {
            true => "search_query",
            false => "search_document",
        };

        let body = json!({
            "texts": data.texts,
            "input_type": input_type,
        });

        let mut request_data = RequestData::new("", body);
        self.patch_request_data(&mut request_data);
        let RequestData {
            url: _,
            headers,
            body,
        } = request_data;

        let builder = aws_fetch(
            client,
            &AwsCredentials {
                access_key_id,
                secret_access_key,
                region,
                session_token,
            },
            AwsRequest {
                method: Method::POST,
                host,
                service: "bedrock".into(),
                uri,
                querystring: "".into(),
                headers,
                body: body.to_string(),
            },
        )?;

        Ok(builder)
    }
}

/// Attempt to fetch fresh AWS credentials by calling `aws configure export-credentials`.
/// Reads profile from BEDROCK_AWS_PROFILE env var (first) or AWS_PROFILE (fallback).
/// Returns None if the aws CLI call fails or no profile env var is set — callers fall back
/// to the standard config_get_fn chain (env vars / config file).
fn fetch_bedrock_creds_from_cli(config_profile: Option<&str>) -> Option<(String, String, Option<String>)> {
    let profile = std::env::var("BEDROCK_AWS_PROFILE")
        .or_else(|_| std::env::var("AWS_PROFILE"))
        .ok()
        .or_else(|| config_profile.map(|s| s.to_string()))?;

    let output = std::process::Command::new("aws")
        .args([
            "configure",
            "export-credentials",
            "--profile",
            &profile,
            "--format",
            "env-no-export",
        ])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let text = String::from_utf8_lossy(&output.stdout);
    let mut akid = None::<String>;
    let mut sak = None::<String>;
    let mut st = None::<String>;

    for line in text.lines() {
        if let Some((k, v)) = line.split_once('=') {
            match k.trim() {
                "AWS_ACCESS_KEY_ID"     => akid = Some(v.trim().to_string()),
                "AWS_SECRET_ACCESS_KEY" => sak  = Some(v.trim().to_string()),
                "AWS_SESSION_TOKEN"     => st   = Some(v.trim().to_string()),
                _ => {}
            }
        }
    }

    Some((akid?, sak?, st))
}

#[async_trait::async_trait]
impl Client for BedrockClient {
    client_common_fns!();

    async fn chat_completions_inner(
        &self,
        client: &ReqwestClient,
        data: ChatCompletionsData,
    ) -> Result<ChatCompletionsOutput> {
        let (builder, category) = self.chat_completions_builder(client, data)?;
        match category {
            BedrockModelCategory::Converse => chat_completions(builder).await,
            BedrockModelCategory::OpenAI => {
                responses_api_chat_completions(builder, &self.model).await
            }
        }
    }

    async fn chat_completions_streaming_inner(
        &self,
        client: &ReqwestClient,
        handler: &mut SseHandler,
        data: ChatCompletionsData,
    ) -> Result<()> {
        let (builder, category) = self.chat_completions_builder(client, data)?;
        match category {
            BedrockModelCategory::Converse => {
                chat_completions_streaming(builder, handler).await
            }
            BedrockModelCategory::OpenAI => {
                responses_api_streaming(builder, handler, &self.model).await
            }
        }
    }

    async fn embeddings_inner(
        &self,
        client: &ReqwestClient,
        data: &EmbeddingsData,
    ) -> Result<EmbeddingsOutput> {
        let builder = self.embeddings_builder(client, data)?;
        embeddings(builder).await
    }
}

async fn chat_completions(builder: RequestBuilder) -> Result<ChatCompletionsOutput> {
    let res = builder.send().await?;
    let status = res.status();
    let data: Value = response_to_json(res).await?;

    if !status.is_success() {
        catch_error(&data, status.as_u16())?;
    }

    debug!("non-stream-data: {data}");
    extract_chat_completions(&data)
}

async fn chat_completions_streaming(
    builder: RequestBuilder,
    handler: &mut SseHandler,
) -> Result<()> {
    let res = builder.send().await?;
    let status = res.status();
    if !status.is_success() {
        let data: Value = response_to_json(res).await?;
        catch_error(&data, status.as_u16())?;
        bail!("Invalid response data: {data}");
    }

    let mut function_name = String::new();
    let mut function_arguments = String::new();
    let mut function_id = String::new();
    let mut reasoning_state = 0;

    let mut stream = res.bytes_stream();
    let mut buffer = BytesMut::new();
    let mut decoder = MessageFrameDecoder::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        buffer.extend_from_slice(&chunk);
        while let DecodedFrame::Complete(message) = decoder.decode_frame(&mut buffer)? {
            let response_headers = parse_response_headers(&message)?;
            let message_type = response_headers.message_type.as_str();
            let smithy_type = response_headers.smithy_type.as_str();
            match (message_type, smithy_type) {
                ("event", _) => {
                    let data: Value = serde_json::from_slice(message.payload())?;
                    debug!("stream-data: {smithy_type} {data}");
                    match smithy_type {
                        "contentBlockStart" => {
                            if let Some(tool_use) = data["start"]["toolUse"].as_object() {
                                if let (Some(id), Some(name)) = (
                                    json_str_from_map(tool_use, "toolUseId"),
                                    json_str_from_map(tool_use, "name"),
                                ) {
                                    if !function_name.is_empty() {
                                        if function_arguments.is_empty() {
                                            function_arguments = String::from("{}");
                                        }
                                        let arguments: Value =
                                        function_arguments.parse().with_context(|| {
                                            format!("Tool call '{function_name}' have non-JSON arguments '{function_arguments}'")
                                        })?;
                                        handler.tool_call(ToolCall::new(
                                            function_name.clone(),
                                            arguments,
                                            Some(function_id.clone()),
                                        ))?;
                                    }
                                    function_arguments.clear();
                                    function_name = name.into();
                                    function_id = id.into();
                                }
                            }
                        }
                        "contentBlockDelta" => {
                            if let Some(text) = data["delta"]["text"].as_str() {
                                handler.text(text)?;
                            } else if let Some(text) =
                                data["delta"]["reasoningContent"]["text"].as_str()
                            {
                                if reasoning_state == 0 {
                                    handler.text("<think>\n")?;
                                    reasoning_state = 1;
                                }
                                handler.text(text)?;
                            } else if let Some(input) = data["delta"]["toolUse"]["input"].as_str() {
                                function_arguments.push_str(input);
                            }
                        }
                        "contentBlockStop" => {
                            if reasoning_state == 1 {
                                handler.text("\n</think>\n\n")?;
                                reasoning_state = 0;
                            }
                            if !function_name.is_empty() {
                                if function_arguments.is_empty() {
                                    function_arguments = String::from("{}");
                                }
                                let arguments: Value = function_arguments.parse().with_context(|| {
                                    format!("Tool call '{function_name}' have non-JSON arguments '{function_arguments}'")
                                })?;
                                handler.tool_call(ToolCall::new(
                                    function_name.clone(),
                                    arguments,
                                    Some(function_id.clone()),
                                ))?;
                            }
                        }
                        _ => {}
                    }
                }
                ("exception", _) => {
                    let payload = base64_decode(message.payload())?;
                    let data = String::from_utf8_lossy(&payload);

                    bail!("Invalid response data: {data} (smithy_type: {smithy_type})")
                }
                _ => {
                    bail!("Unrecognized message, message_type: {message_type}, smithy_type: {smithy_type}",);
                }
            }
        }
    }
    Ok(())
}

async fn embeddings(builder: RequestBuilder) -> Result<EmbeddingsOutput> {
    let res = builder.send().await?;
    let status = res.status();
    let data: Value = response_to_json(res).await?;

    if !status.is_success() {
        catch_error(&data, status.as_u16())?;
    }

    let res_body: EmbeddingsResBody =
        serde_json::from_value(data).context("Invalid embeddings data")?;
    Ok(res_body.embeddings)
}

#[derive(Deserialize)]
struct EmbeddingsResBody {
    embeddings: Vec<Vec<f32>>,
}

fn build_chat_completions_body(data: ChatCompletionsData, model: &Model) -> Result<Value> {
    let ChatCompletionsData {
        mut messages,
        temperature,
        top_p,
        functions,
        stream: _,
    } = data;

    let system_message = extract_system_message(&mut messages);

    let mut network_image_urls = vec![];

    let messages_len = messages.len();
    let messages: Vec<Value> = messages
        .into_iter()
        .enumerate()
        .flat_map(|(i, message)| {
            let Message { role, content } = message;
            match content {
                MessageContent::Text(text) if role.is_assistant() && i != messages_len - 1 => {
                    vec![json!({ "role": role, "content": [ { "text": strip_think_tag(&text) } ] })]
                }
                MessageContent::Text(text) => vec![json!({
                    "role": role,
                    "content": [
                        {
                            "text": text,
                        }
                    ],
                })],
                MessageContent::Array(list) => {
                    let content: Vec<_> = list
                        .into_iter()
                        .map(|item| match item {
                            MessageContentPart::Text { text } => {
                                json!({"text": text})
                            }
                            MessageContentPart::ImageUrl {
                                image_url: ImageUrl { url },
                            } => {
                                if let Some((mime_type, data)) = url
                                    .strip_prefix("data:")
                                    .and_then(|v| v.split_once(";base64,"))
                                {
                                    json!({
                                        "image": {
                                            "format": mime_type.replace("image/", ""),
                                            "source": {
                                                "bytes": data,
                                            }
                                        }
                                    })
                                } else {
                                    network_image_urls.push(url.clone());
                                    json!({ "url": url })
                                }
                            }
                        })
                        .collect();
                    vec![json!({
                        "role": role,
                        "content": content,
                    })]
                }
                MessageContent::ToolCalls(MessageContentToolCalls {
                    tool_results, text, ..
                }) => {
                    let mut assistant_parts = vec![];
                    let mut user_parts = vec![];
                    if !text.is_empty() {
                        assistant_parts.push(json!({
                            "text": text,
                        }))
                    }
                    for tool_result in tool_results {
                        assistant_parts.push(json!({
                            "toolUse": {
                                "toolUseId": tool_result.call.id,
                                "name": tool_result.call.name,
                                "input": tool_result.call.arguments,
                            }
                        }));
                        user_parts.push(json!({
                            "toolResult": {
                                "toolUseId": tool_result.call.id,
                                "content": [
                                    {
                                        "json": ensure_json_object(tool_result.output),
                                    }
                                ]
                            }
                        }));
                    }
                    vec![
                        json!({
                            "role": "assistant",
                            "content": assistant_parts,
                        }),
                        json!({
                            "role": "user",
                            "content": user_parts,
                        }),
                    ]
                }
            }
        })
        .collect();

    if !network_image_urls.is_empty() {
        bail!(
            "The model does not support network images: {:?}",
            network_image_urls
        );
    }

    let mut body = json!({
        "inferenceConfig": {},
        "messages": messages,
    });
    if let Some(v) = system_message {
        body["system"] = json!([
            {
                "text": v,
            }
        ])
    }

    if let Some(v) = model.max_tokens_param() {
        body["inferenceConfig"]["maxTokens"] = v.into();
    }
    if let Some(v) = temperature {
        body["inferenceConfig"]["temperature"] = v.into();
    }
    if let Some(v) = top_p {
        body["inferenceConfig"]["topP"] = v.into();
    }
    if let Some(functions) = functions {
        let tools: Vec<_> = functions
            .iter()
            .map(|v| {
                json!({
                    "toolSpec": {
                        "name": v.name,
                        "description": v.description,
                        "inputSchema": {
                            "json": v.parameters,
                        },
                    }
                })
            })
            .collect();
        body["toolConfig"] = json!({
            "tools": tools,
        })
    }

    // Smart prompt caching: add cachePoint markers when caching is likely beneficial
    let has_system = body.get("system").is_some();
    let has_multi_messages = body["messages"].as_array().map_or(false, |a| a.len() > 1);
    let has_tools = body.get("toolConfig").is_some();
    let should_cache = has_system || has_multi_messages || has_tools;

    if should_cache {
        let cache_point = match bedrock_cache_ttl(model) {
            Some(ttl) => json!({"cachePoint": {"type": "default", "ttl": ttl}}),
            None => json!({"cachePoint": {"type": "default"}}),
        };

        // Add cachePoint after system text
        if let Some(system_arr) = body.get_mut("system").and_then(|s| s.as_array_mut()) {
            system_arr.push(cache_point.clone());
        }

        // Add cachePoint at end of last message's content for multi-turn caching
        if has_multi_messages {
            if let Some(content_arr) = body.get_mut("messages")
                .and_then(|m| m.as_array_mut())
                .and_then(|arr| arr.last_mut())
                .and_then(|msg| msg.get_mut("content"))
                .and_then(|c| c.as_array_mut())
            {
                content_arr.push(cache_point.clone());
            }
        }

        // Add cachePoint after tools
        if let Some(tools_arr) = body.get_mut("toolConfig")
            .and_then(|tc| tc.get_mut("tools"))
            .and_then(|t| t.as_array_mut())
        {
            tools_arr.push(cache_point);
        }
    }

    Ok(body)
}

/// Determine the cache TTL for Bedrock prompt caching based on model name.
/// Returns `Some("1h")` for recent Claude models that support 1-hour TTL,
/// or `None` for older models (which use the default 5-minute TTL, represented
/// by omitting the `ttl` field entirely).
fn bedrock_cache_ttl(model: &Model) -> Option<&'static str> {
    let name = model.name().to_lowercase();
    if name.contains("4-5")
        || name.contains("4-6")
        || name.contains("opus-4")
        || name.contains("sonnet-4")
        || name.contains("haiku-4")
    {
        Some("1h")
    } else {
        None
    }
}

fn extract_chat_completions(data: &Value) -> Result<ChatCompletionsOutput> {
    let mut text = String::new();
    let mut reasoning = None;
    let mut tool_calls = vec![];
    if let Some(array) = data["output"]["message"]["content"].as_array() {
        for item in array {
            if let Some(v) = item["text"].as_str() {
                if !text.is_empty() {
                    text.push_str("\n\n");
                }
                text.push_str(v);
            } else if let Some(reasoning_text) =
                item["reasoningContent"]["reasoningText"].as_object()
            {
                if let Some(text) = json_str_from_map(reasoning_text, "text") {
                    reasoning = Some(text.to_string());
                }
            } else if let Some(tool_use) = item["toolUse"].as_object() {
                if let (Some(id), Some(name), Some(input)) = (
                    json_str_from_map(tool_use, "toolUseId"),
                    json_str_from_map(tool_use, "name"),
                    tool_use.get("input"),
                ) {
                    tool_calls.push(ToolCall::new(
                        name.to_string(),
                        input.clone(),
                        Some(id.to_string()),
                    ))
                }
            }
        }
    }

    if let Some(reasoning) = reasoning {
        text = format!("<think>\n{reasoning}\n</think>\n\n{text}")
    }

    if text.is_empty() && tool_calls.is_empty() {
        bail!("Invalid response data: {data}");
    }

    let output = ChatCompletionsOutput {
        text,
        tool_calls,
        id: None,
        input_tokens: data["usage"]["inputTokens"].as_u64(),
        output_tokens: data["usage"]["outputTokens"].as_u64(),
        extra: None,
    };
    Ok(output)
}

#[derive(Debug)]
struct AwsCredentials {
    access_key_id: String,
    secret_access_key: String,
    region: String,
    session_token: Option<String>,
}

#[derive(Debug)]
struct AwsRequest {
    method: Method,
    host: String,
    service: String,
    uri: String,
    querystring: String,
    headers: IndexMap<String, String>,
    body: String,
}

fn aws_fetch(
    client: &ReqwestClient,
    credentials: &AwsCredentials,
    request: AwsRequest,
) -> Result<RequestBuilder> {
    let AwsRequest {
        method,
        host,
        service,
        uri,
        querystring,
        mut headers,
        body,
    } = request;
    let region = &credentials.region;

    let endpoint = format!("https://{host}{uri}");

    let now: DateTime<Utc> = Utc::now();
    let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
    let date_stamp = amz_date[0..8].to_string();
    headers.insert("host".into(), host.clone());
    headers.insert("content-type".into(), "application/json".into());
    headers.insert("x-amz-date".into(), amz_date.clone());
    if let Some(token) = credentials.session_token.clone() {
        headers.insert("x-amz-security-token".into(), token);
    }

    let mut sorted_keys: Vec<&String> = headers.keys().collect();
    sorted_keys.sort();

    let canonical_headers = sorted_keys
        .iter()
        .map(|key| format!("{}:{}\n", key, headers[key.as_str()]))
        .collect::<Vec<_>>()
        .join("");

    let signed_headers = sorted_keys
        .iter()
        .map(|key| key.as_str())
        .collect::<Vec<_>>()
        .join(";");

    let payload_hash = sha256(&body);

    let canonical_request = format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        method,
        encode_uri(&uri),
        querystring,
        canonical_headers,
        signed_headers,
        payload_hash
    );

    let algorithm = "AWS4-HMAC-SHA256";
    let credential_scope = format!("{date_stamp}/{region}/{service}/aws4_request");
    let string_to_sign = format!(
        "{}\n{}\n{}\n{}",
        algorithm,
        amz_date,
        credential_scope,
        sha256(&canonical_request)
    );

    let signing_key = gen_signing_key(
        &credentials.secret_access_key,
        &date_stamp,
        region,
        &service,
    );
    let signature = hmac_sha256(&signing_key, &string_to_sign);
    let signature = hex_encode(&signature);

    let authorization_header = format!(
        "{} Credential={}/{}, SignedHeaders={}, Signature={}",
        algorithm, credentials.access_key_id, credential_scope, signed_headers, signature
    );

    headers.insert("authorization".into(), authorization_header);

    debug!("Request {endpoint} {body}");

    let mut request_builder = client.request(method, endpoint).body(body);

    for (key, value) in &headers {
        if key == "host" {
            continue;
        }
        request_builder = request_builder.header(key.as_str(), value.as_str());
    }

    Ok(request_builder)
}

fn gen_signing_key(key: &str, date_stamp: &str, region: &str, service: &str) -> Vec<u8> {
    let k_date = hmac_sha256(format!("AWS4{key}").as_bytes(), date_stamp);
    let k_region = hmac_sha256(&k_date, region);
    let k_service = hmac_sha256(&k_region, service);
    hmac_sha256(&k_service, "aws4_request")
}

/// Build a request body for the OpenAI Responses API format.
/// Converts ChatCompletionsData messages into the "input" field expected by /v1/responses.
fn build_responses_api_body(data: ChatCompletionsData, model: &Model) -> Value {
    let ChatCompletionsData {
        messages,
        temperature,
        top_p,
        functions,
        stream,
    } = data;

    // Convert messages to the Responses API "input" format.
    // The Responses API accepts messages as an array of {role, content} objects,
    // similar to chat completions but with "input" key instead of "messages".
    // System messages become the "instructions" field.
    let mut instructions = String::new();
    let mut input: Vec<Value> = Vec::new();

    for message in messages {
        let Message { role, content } = message;
        match role {
            MessageRole::System => {
                // System messages go into "instructions" field
                if let MessageContent::Text(text) = content {
                    if !instructions.is_empty() {
                        instructions.push('\n');
                    }
                    instructions.push_str(&text);
                }
            }
            _ => {
                // User and assistant messages go into "input" array
                match content {
                    MessageContent::Text(text) => {
                        input.push(json!({
                            "role": role,
                            "content": text,
                        }));
                    }
                    MessageContent::Array(parts) => {
                        // Multi-part content (text + images)
                        let content_parts: Vec<Value> = parts.into_iter().map(|part| {
                            match part {
                                MessageContentPart::Text { text } => json!({"type": "input_text", "text": text}),
                                MessageContentPart::ImageUrl { image_url } => json!({"type": "input_image", "image_url": image_url.url}),
                            }
                        }).collect();
                        input.push(json!({
                            "role": role,
                            "content": content_parts,
                        }));
                    }
                    MessageContent::ToolCalls(MessageContentToolCalls {
                        tool_results,
                        ..
                    }) => {
                        // Convert tool calls to Responses API format:
                        // Each ToolResult generates a function_call item (the assistant's call)
                        // followed by a function_call_output item (the tool's response).
                        for tool_result in tool_results {
                            let call_id = tool_result.call.id.clone().unwrap_or_default();
                            input.push(json!({
                                "type": "function_call",
                                "name": tool_result.call.name,
                                "call_id": call_id,
                                "arguments": tool_result.call.arguments.to_string(),
                            }));
                            input.push(json!({
                                "type": "function_call_output",
                                "call_id": call_id,
                                "output": tool_result.output.to_string(),
                            }));
                        }
                    }
                }
            }
        }
    }

    let mut body = json!({
        "model": model.real_name(),
        "input": input,
        "stream": stream,
    });

    if !instructions.is_empty() {
        body["instructions"] = json!(instructions);
    }
    if let Some(v) = temperature {
        body["temperature"] = v.into();
    }
    if let Some(v) = top_p {
        body["top_p"] = v.into();
    }
    if let Some(v) = model.max_tokens_param() {
        body["max_output_tokens"] = v.into();
    }
    if let Some(functions) = functions {
        body["tools"] = functions
            .iter()
            .map(|v| {
                json!({
                    "type": "function",
                    "name": v.name,
                    "description": v.description,
                    "parameters": v.parameters,
                    "strict": false,
                })
            })
            .collect();
    }

    body
}

/// Handle non-streaming Responses API response.
/// Extracts text from the response output array.
async fn responses_api_chat_completions(
    builder: RequestBuilder,
    _model: &Model,
) -> Result<ChatCompletionsOutput> {
    let res = builder.send().await?;
    let status = res.status();
    let data: Value = response_to_json(res).await?;
    if !status.is_success() {
        catch_error(&data, status.as_u16())?;
    }

    debug!("responses-api-data: {data}");

    // Extract text from Responses API format:
    // {"output": [{"type": "message", "content": [{"type": "output_text", "text": "..."}]}]}
    let mut text = String::new();
    let mut tool_calls = vec![];
    if let Some(output_arr) = data["output"].as_array() {
        for item in output_arr {
            if let Some(content_arr) = item["content"].as_array() {
                for content_item in content_arr {
                    if let Some(t) = content_item["text"].as_str() {
                        if !text.is_empty() {
                            text.push_str("\n\n");
                        }
                        text.push_str(t);
                    }
                }
            }
            // Parse tool call items: {"type": "function_call", "name": "...", "arguments": "...", "call_id": "..."}
            if item["type"].as_str() == Some("function_call") {
                if let (Some(name), Some(arguments_str)) = (
                    item["name"].as_str(),
                    item["arguments"].as_str(),
                ) {
                    let call_id = item["call_id"].as_str().map(|s| s.to_string());
                    let arguments: Value = arguments_str.parse().with_context(|| {
                        format!("Tool call '{name}' has non-JSON arguments '{arguments_str}'")
                    })?;
                    tool_calls.push(ToolCall::new(
                        name.to_string(),
                        arguments,
                        call_id,
                    ));
                }
            }
        }
    }

    if text.is_empty() && tool_calls.is_empty() {
        // GPT-5.5 may return empty output due to strict schema constraints or other issues.
        // Return empty response instead of erroring, allowing retry/graceful handling upstream.
        return Ok(ChatCompletionsOutput {
            text: String::new(),
            tool_calls: vec![],
            id: data["id"].as_str().map(|s| s.to_string()),
            input_tokens: data["usage"]["input_tokens"].as_u64(),
            output_tokens: data["usage"]["output_tokens"].as_u64(),
            extra: data.get("extra_fields").cloned(),
        });
    }

    let output = ChatCompletionsOutput {
        text,
        tool_calls,
        id: data["id"].as_str().map(|s| s.to_string()),
        input_tokens: data["usage"]["input_tokens"].as_u64(),
        output_tokens: data["usage"]["output_tokens"].as_u64(),
        extra: data.get("extra_fields").cloned(),
    };
    Ok(output)
}

/// Handle streaming Responses API response.
/// Parses SSE events for text deltas from `response.output_text.delta` events.
async fn responses_api_streaming(
    builder: RequestBuilder,
    handler: &mut SseHandler,
    _model: &Model,
) -> Result<()> {
    let mut function_name = String::new();
    let mut function_arguments = String::new();
    let mut function_call_id = String::new();
    let handle = |message: SseMmessage| -> Result<bool> {
        // The Responses API streaming uses SSE with event types like:
        // event: response.output_text.delta
        // data: {"type":"response.output_text.delta","delta":"text chunk"}
        //
        // event: response.completed
        // data: {"type":"response.completed",...}
        let data: Value = match serde_json::from_str(&message.data) {
            Ok(v) => v,
            Err(_) => return Ok(false),
        };

        let event_type = data["type"].as_str().unwrap_or("");
        match event_type {
            "response.output_text.delta" => {
                if let Some(delta) = data["delta"].as_str() {
                    handler.text(delta)?;
                }
            }
            "response.function_call_arguments.delta" => {
                if let Some(delta) = data["delta"].as_str() {
                    function_arguments.push_str(delta);
                }
                if function_name.is_empty() {
                    if let Some(name) = data["name"].as_str() {
                        function_name = name.to_string();
                    }
                }
                if function_call_id.is_empty() {
                    if let Some(id) = data["call_id"].as_str() {
                        function_call_id = id.to_string();
                    }
                }
            }
            "response.function_call_arguments.done" => {
                let name = data["name"].as_str().unwrap_or(&function_name).to_string();
                let args_str = data["arguments"].as_str().unwrap_or(&function_arguments);
                let arguments: Value = args_str.parse().unwrap_or(json!({}));
                let call_id = data["call_id"].as_str().map(|s| s.to_string()).or_else(|| if function_call_id.is_empty() { None } else { Some(function_call_id.clone()) });
                handler.tool_call(ToolCall::new(name, arguments, call_id))?;
                function_name.clear();
                function_arguments.clear();
                function_call_id.clear();
            }
            "response.completed" | "response.done" | "response.finished" => {
                return Ok(true);
            }
            _ => {
                // Ignore other event types (response.created, response.output_item.added, etc.)
            }
        }

        Ok(false)
    };

    sse_stream(builder, handle).await
}

/// Ensures a serde_json::Value is a JSON object, as required by the Bedrock
/// Converse API for `toolResult.content[].json`.  Non-object values (strings,
/// numbers, arrays, booleans, nulls) are wrapped in `{"result": <value>}`.
fn ensure_json_object(value: Value) -> Value {
    match value {
        Value::Object(_) => value,
        _ => json!({"result": value}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::function::{ToolCall, ToolResult};

    /// Helper: build a ChatCompletionsData whose only non-system message is a
    /// ToolCalls message carrying a single tool result with the given `output`.
    fn build_body_with_tool_output(output: Value) -> Value {
        let call = ToolCall::new("test_fn".into(), json!({}), Some("call_1".into()));
        let tr = ToolResult::new(call, output);
        let tc = MessageContentToolCalls::new(vec![tr], String::new());

        let messages = vec![
            Message::new(MessageRole::System, MessageContent::Text("sys".into())),
            Message::new(MessageRole::User, MessageContent::ToolCalls(tc)),
        ];
        let data = ChatCompletionsData { messages, temperature: None, top_p: None, functions: None, stream: false };
        let model = Model::new("bedrock", "test-model");
        build_chat_completions_body(data, &model).unwrap()
    }

    fn extract_tool_json(body: &Value) -> &Value {
        // messages[0] = user (ToolCalls assistant), messages[1] = user (toolResult)
        // The second message in the body messages array has the toolResult
        &body["messages"][1]["content"][0]["toolResult"]["content"][0]["json"]
    }

    #[test]
    fn test_bedrock_tool_result_json_string_wrapped() {
        let body = build_body_with_tool_output(json!("DONE"));
        let j = extract_tool_json(&body);
        assert!(j.is_object(), "string tool output must be wrapped: {j}");
        assert_eq!(j["result"], json!("DONE"));
    }

    #[test]
    fn test_bedrock_tool_result_json_object_passthrough() {
        let orig = json!({"key": "value"});
        let body = build_body_with_tool_output(orig.clone());
        let j = extract_tool_json(&body);
        assert!(j.is_object());
        assert_eq!(*j, orig);
    }
}

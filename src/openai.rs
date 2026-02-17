use crate::config::AgentPaths;
use crate::connect::{self, ConnectMode, ConnectProvider};
use crate::usage::{self, UsageEvent};
use anyhow::{Context, Result, anyhow, bail};
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::path::PathBuf;
use tokio::process::Command;
use uuid::Uuid;

const ZHIPU_GENERAL_CHAT_ENDPOINT: &str = "https://open.bigmodel.cn/api/paas/v4/chat/completions";
const ZHIPU_CODING_CHAT_ENDPOINT: &str =
    "https://open.bigmodel.cn/api/coding/paas/v4/chat/completions";

#[derive(Debug, Clone, Serialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: "system".to_string(),
            content: content.into(),
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".to_string(),
            content: content.into(),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: "assistant".to_string(),
            content: content.into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct OpenAIClient {
    backend: ModelBackend,
    usage_file: Option<PathBuf>,
}

#[derive(Debug, Clone)]
enum ModelBackend {
    ApiCompatible {
        http: reqwest::Client,
        model: String,
        endpoint: String,
        provider: ConnectProvider,
    },
    CodexExec {
        model: Option<String>,
    },
}

impl OpenAIClient {
    pub fn from_paths(paths: &AgentPaths, model_override: Option<String>) -> Result<Self> {
        let usage_file = Some(paths.usage_file.clone());
        let cfg = connect::load(paths).unwrap_or_default();
        let env_model = env::var("GOLDAGENT_MODEL").ok();
        let fallback_model = model_override.clone().or_else(|| match cfg.provider {
            ConnectProvider::OpenAi => cfg.model.clone(),
            _ => env_model.clone(),
        });

        match cfg.mode {
            ConnectMode::OpenAIApi => {
                let provider = cfg.provider.clone();
                let env_api_key = env::var(connect::provider_env_var(&provider)).ok();
                let configured_key = cfg.api_key.clone().or(env_api_key);
                if let Some(api_key) = configured_key.as_deref() {
                    if !api_key.trim().is_empty()
                        && connect::validate_api_key(&provider, api_key).is_ok()
                    {
                        let model =
                            model_override
                                .or(cfg.model)
                                .or(env_model)
                                .unwrap_or_else(|| {
                                    connect::default_model_for_provider(&provider).to_string()
                                });
                        return Self::build_api_backend(api_key, provider, model, usage_file);
                    }
                }
            }
            ConnectMode::CodexLogin => {
                let model = model_override.or(cfg.model).or(env_model);
                return Ok(Self {
                    backend: ModelBackend::CodexExec { model },
                    usage_file,
                });
            }
        }

        Self::from_env_with_usage(fallback_model, usage_file)
    }

    #[allow(dead_code)]
    pub fn from_env(model_override: Option<String>) -> Result<Self> {
        Self::from_env_with_usage(model_override, None)
    }

    fn from_env_with_usage(
        model_override: Option<String>,
        usage_file: Option<PathBuf>,
    ) -> Result<Self> {
        let model = model_override.or_else(|| env::var("GOLDAGENT_MODEL").ok());

        if let Ok(api_key) = env::var("OPENAI_API_KEY") {
            if !api_key.trim().is_empty() {
                let direct_model = model.unwrap_or_else(|| "gpt-4.1-mini".to_string());
                return Self::build_api_backend(
                    &api_key,
                    ConnectProvider::OpenAi,
                    direct_model,
                    usage_file,
                );
            }
        }

        Ok(Self {
            backend: ModelBackend::CodexExec { model },
            usage_file,
        })
    }

    pub async fn chat(&self, messages: &[ChatMessage]) -> Result<String> {
        match &self.backend {
            ModelBackend::ApiCompatible {
                http,
                model,
                endpoint,
                provider,
            } => {
                let output = match provider {
                    ConnectProvider::Anthropic => {
                        chat_via_anthropic_api(http, endpoint, model, messages).await?
                    }
                    ConnectProvider::OpenAi => {
                        chat_via_openai_compatible_api(http, endpoint, model, messages).await?
                    }
                    ConnectProvider::Zhipu => chat_via_zhipu_api(http, model, messages).await?,
                };
                self.record_usage(UsageEvent {
                    model_key: format!("{}:{model}", provider_key(provider)),
                    input_tokens: output.input_tokens,
                    output_tokens: output.output_tokens,
                });
                Ok(output.content)
            }
            ModelBackend::CodexExec { model } => {
                let content = chat_via_codex_exec(messages, model.clone()).await?;
                let model_key = model
                    .as_deref()
                    .map(|m| format!("codex:{m}"))
                    .unwrap_or_else(|| "codex:default".to_string());
                self.record_usage(UsageEvent {
                    model_key,
                    input_tokens: 0,
                    output_tokens: 0,
                });
                Ok(content)
            }
        }
    }

    pub fn backend_label(&self) -> String {
        match &self.backend {
            ModelBackend::ApiCompatible {
                provider, model, ..
            } => format!("{} / API / {model}", connect::provider_label(provider)),
            ModelBackend::CodexExec { model } => match model {
                Some(model) => format!("OpenAI / 登录态(Codex) / {model}"),
                None => "OpenAI / 登录态(Codex) / 默认模型".to_string(),
            },
        }
    }

    pub fn usage_model_key(&self) -> String {
        match &self.backend {
            ModelBackend::ApiCompatible {
                provider, model, ..
            } => {
                format!("{}:{model}", provider_key(provider))
            }
            ModelBackend::CodexExec { model } => model
                .as_deref()
                .map(|m| format!("codex:{m}"))
                .unwrap_or_else(|| "codex:default".to_string()),
        }
    }

    fn build_api_backend(
        api_key: &str,
        provider: ConnectProvider,
        model: String,
        usage_file: Option<PathBuf>,
    ) -> Result<Self> {
        let endpoint = api_endpoint_for_provider(&provider)?;
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        match provider {
            ConnectProvider::OpenAi | ConnectProvider::Zhipu => {
                headers.insert(
                    AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {api_key}"))
                        .map_err(|_| anyhow!("Failed to encode API key header"))?,
                );
            }
            ConnectProvider::Anthropic => {
                headers.insert(
                    HeaderName::from_static("x-api-key"),
                    HeaderValue::from_str(api_key)
                        .map_err(|_| anyhow!("Failed to encode Anthropic API key header"))?,
                );
                headers.insert(
                    HeaderName::from_static("anthropic-version"),
                    HeaderValue::from_static("2023-06-01"),
                );
            }
        }

        let http = reqwest::Client::builder()
            .default_headers(headers)
            .build()?;
        Ok(Self {
            backend: ModelBackend::ApiCompatible {
                http,
                model,
                endpoint,
                provider,
            },
            usage_file,
        })
    }

    fn record_usage(&self, event: UsageEvent) {
        if let Some(path) = &self.usage_file {
            let _ = usage::record(path, &event);
        }
    }
}

async fn chat_via_openai_compatible_api(
    http: &reqwest::Client,
    endpoint: &str,
    model: &str,
    messages: &[ChatMessage],
) -> Result<ChatApiOutput> {
    let body = ChatCompletionRequest {
        model: model.to_string(),
        messages: messages.to_vec(),
        temperature: 0.2,
    };

    let response = http
        .post(endpoint)
        .json(&body)
        .send()
        .await
        .with_context(|| format!("Failed to call API: {endpoint}"))?;

    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        bail!("API error {status}: {text}");
    }

    let parsed: ChatCompletionResponse = response
        .json()
        .await
        .context("Failed to parse OpenAI chat completion response")?;

    let content = parsed
        .choices
        .first()
        .and_then(|choice| choice.message.content.clone())
        .ok_or_else(|| anyhow!("OpenAI response did not include a message content"))?;

    let input_tokens = parsed
        .usage
        .as_ref()
        .map(|usage| usage.prompt_tokens)
        .unwrap_or(0);
    let output_tokens = parsed
        .usage
        .as_ref()
        .map(|usage| usage.completion_tokens)
        .unwrap_or(0);

    Ok(ChatApiOutput {
        content,
        input_tokens,
        output_tokens,
    })
}

async fn chat_via_zhipu_api(
    http: &reqwest::Client,
    model: &str,
    messages: &[ChatMessage],
) -> Result<ChatApiOutput> {
    match chat_via_openai_compatible_api(http, ZHIPU_CODING_CHAT_ENDPOINT, model, messages).await {
        Ok(output) => Ok(output),
        Err(coding_err) => {
            let coding_text = coding_err.to_string();
            if !looks_like_zhipu_quota_1113(&coding_text) {
                return Err(coding_err);
            }

            match chat_via_openai_compatible_api(http, ZHIPU_GENERAL_CHAT_ENDPOINT, model, messages)
                .await
            {
                Ok(output) => Ok(output),
                Err(general_err) => {
                    bail!(
                        "智谱 API 调用失败：Coding 端点返回 1113（余额不足或资源包不可用），已自动尝试通用端点但仍失败。\nCoding 端点: {coding_text}\n通用端点: {}",
                        general_err
                    );
                }
            }
        }
    }
}

fn looks_like_zhipu_quota_1113(err: &str) -> bool {
    err.contains("\"code\":\"1113\"")
        || err.contains("\"code\":1113")
        || (err.contains("1113") && err.contains("余额不足"))
        || (err.contains("1113") && err.contains("资源包"))
}

async fn chat_via_anthropic_api(
    http: &reqwest::Client,
    endpoint: &str,
    model: &str,
    messages: &[ChatMessage],
) -> Result<ChatApiOutput> {
    let mut system_parts = Vec::new();
    let mut anthropic_messages = Vec::new();

    for message in messages {
        match message.role.as_str() {
            "system" => system_parts.push(message.content.clone()),
            "user" | "assistant" => anthropic_messages.push(AnthropicMessage {
                role: message.role.clone(),
                content: message.content.clone(),
            }),
            _ => {}
        }
    }

    if anthropic_messages.is_empty() {
        bail!("Anthropic 请求缺少 user/assistant 消息");
    }

    let body = AnthropicMessagesRequest {
        model: model.to_string(),
        max_tokens: 2_048,
        temperature: 0.2,
        system: if system_parts.is_empty() {
            None
        } else {
            Some(system_parts.join("\n\n"))
        },
        messages: anthropic_messages,
    };

    let response = http
        .post(endpoint)
        .json(&body)
        .send()
        .await
        .with_context(|| format!("Failed to call API: {endpoint}"))?;

    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        bail!("API error {status}: {text}");
    }

    let parsed: AnthropicMessagesResponse = response
        .json()
        .await
        .context("Failed to parse Anthropic messages response")?;

    let content = parsed
        .content
        .iter()
        .filter_map(|block| block.text.clone())
        .collect::<Vec<_>>()
        .join("");

    if content.trim().is_empty() {
        bail!("Anthropic 响应未返回文本内容");
    }

    let input_tokens = parsed
        .usage
        .as_ref()
        .map(|usage| usage.input_tokens)
        .unwrap_or(0);
    let output_tokens = parsed
        .usage
        .as_ref()
        .map(|usage| usage.output_tokens)
        .unwrap_or(0);

    Ok(ChatApiOutput {
        content,
        input_tokens,
        output_tokens,
    })
}

fn provider_key(provider: &ConnectProvider) -> &'static str {
    match provider {
        ConnectProvider::OpenAi => "openai",
        ConnectProvider::Anthropic => "anthropic",
        ConnectProvider::Zhipu => "zhipu",
    }
}

fn api_endpoint_for_provider(provider: &ConnectProvider) -> Result<String> {
    match provider {
        ConnectProvider::OpenAi => Ok("https://api.openai.com/v1/chat/completions".to_string()),
        ConnectProvider::Zhipu => Ok(ZHIPU_CODING_CHAT_ENDPOINT.to_string()),
        ConnectProvider::Anthropic => Ok("https://api.anthropic.com/v1/messages".to_string()),
    }
}

async fn chat_via_codex_exec(messages: &[ChatMessage], model: Option<String>) -> Result<String> {
    let output_file = env::temp_dir().join(format!("goldagent-codex-{}.txt", Uuid::new_v4()));
    let prompt = build_codex_prompt(messages);

    let mut cmd = Command::new("codex");
    cmd.arg("exec")
        .arg("--skip-git-repo-check")
        .arg("--ephemeral")
        .arg("--sandbox")
        .arg("read-only")
        .arg("--output-last-message")
        .arg(&output_file);

    if let Some(model) = model {
        cmd.arg("--model").arg(model);
    }
    cmd.arg(prompt);

    let output = cmd
        .output()
        .await
        .context("Failed to execute `codex`. Install Codex CLI or set OPENAI_API_KEY.")?;

    if !output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "Codex auth mode failed.\nRun `codex login` first or set OPENAI_API_KEY.\nstdout:\n{}\nstderr:\n{}",
            stdout,
            stderr
        );
    }

    let response = fs::read_to_string(&output_file)
        .with_context(|| format!("Failed to read Codex output file {}", output_file.display()))?;
    let _ = fs::remove_file(&output_file);

    let trimmed = response.trim().to_string();
    if trimmed.is_empty() {
        bail!("Codex returned an empty response.");
    }
    Ok(trimmed)
}

fn build_codex_prompt(messages: &[ChatMessage]) -> String {
    let mut prompt = String::from(
        "You are GoldAgent.\nReturn only the final assistant response text, no extra wrappers.\n\nConversation:\n",
    );

    for message in messages {
        let role = match message.role.as_str() {
            "system" => "System",
            "user" => "User",
            "assistant" => "Assistant",
            _ => "Message",
        };
        prompt.push_str(&format!("{role}:\n{}\n\n", message.content));
    }

    prompt
}

#[derive(Debug, Serialize)]
struct ChatCompletionRequest {
    model: String,
    messages: Vec<ChatMessage>,
    temperature: f32,
}

#[derive(Debug, Deserialize)]
struct ChatCompletionResponse {
    choices: Vec<ChatChoice>,
    usage: Option<ChatUsage>,
}

#[derive(Debug, Deserialize)]
struct ChatChoice {
    message: ChatResponseMessage,
}

#[derive(Debug, Deserialize)]
struct ChatResponseMessage {
    content: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ChatUsage {
    prompt_tokens: u64,
    completion_tokens: u64,
}

#[derive(Debug, Serialize)]
struct AnthropicMessagesRequest {
    model: String,
    max_tokens: u32,
    temperature: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    messages: Vec<AnthropicMessage>,
}

#[derive(Debug, Serialize)]
struct AnthropicMessage {
    role: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct AnthropicMessagesResponse {
    content: Vec<AnthropicContentBlock>,
    usage: Option<AnthropicUsage>,
}

#[derive(Debug, Deserialize)]
struct AnthropicContentBlock {
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AnthropicUsage {
    input_tokens: u64,
    output_tokens: u64,
}

struct ChatApiOutput {
    content: String,
    input_tokens: u64,
    output_tokens: u64,
}

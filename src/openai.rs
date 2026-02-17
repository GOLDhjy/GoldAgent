use anyhow::{anyhow, bail, Context, Result};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use std::fs;
use std::env;
use tokio::process::Command;
use uuid::Uuid;

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
}

#[derive(Debug, Clone)]
enum ModelBackend {
    OpenAIApi {
        http: reqwest::Client,
        model: String,
    },
    CodexExec {
        model: Option<String>,
    },
}

impl OpenAIClient {
    pub fn from_env(model_override: Option<String>) -> Result<Self> {
        let model = model_override.or_else(|| env::var("GOLDAGENT_MODEL").ok());

        if let Ok(api_key) = env::var("OPENAI_API_KEY") {
            if !api_key.trim().is_empty() {
                let direct_model = model.unwrap_or_else(|| "gpt-4.1-mini".to_string());
                let mut headers = HeaderMap::new();
                headers.insert(
                    AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {api_key}"))
                        .map_err(|_| anyhow!("Failed to encode OpenAI API key header"))?,
                );
                headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

                let http = reqwest::Client::builder().default_headers(headers).build()?;
                return Ok(Self {
                    backend: ModelBackend::OpenAIApi {
                        http,
                        model: direct_model,
                    },
                });
            }
        }

        Ok(Self {
            backend: ModelBackend::CodexExec { model },
        })
    }

    pub async fn chat(&self, messages: &[ChatMessage]) -> Result<String> {
        match &self.backend {
            ModelBackend::OpenAIApi { http, model } => chat_via_openai_api(http, model, messages).await,
            ModelBackend::CodexExec { model } => chat_via_codex_exec(messages, model.clone()).await,
        }
    }
}

async fn chat_via_openai_api(
    http: &reqwest::Client,
    model: &str,
    messages: &[ChatMessage],
) -> Result<String> {
    let body = ChatCompletionRequest {
        model: model.to_string(),
        messages: messages.to_vec(),
        temperature: 0.2,
    };

    let response = http
        .post("https://api.openai.com/v1/chat/completions")
        .json(&body)
        .send()
        .await
        .context("Failed to call OpenAI chat completions")?;

    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        bail!("OpenAI API error {status}: {text}");
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

    Ok(content)
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
}

#[derive(Debug, Deserialize)]
struct ChatChoice {
    message: ChatResponseMessage,
}

#[derive(Debug, Deserialize)]
struct ChatResponseMessage {
    content: Option<String>,
}

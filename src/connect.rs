use crate::config::AgentPaths;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::process::Command;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ConnectProvider {
    #[default]
    #[serde(rename = "openai", alias = "open_ai", alias = "open_a_i")]
    OpenAi,
    #[serde(rename = "anthropic")]
    Anthropic,
    #[serde(rename = "zhipu")]
    Zhipu,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectMode {
    CodexLogin,
    OpenAIApi,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectConfig {
    #[serde(default)]
    pub provider: ConnectProvider,
    pub mode: ConnectMode,
    pub model: Option<String>,
    #[serde(default, alias = "openai_api_key")]
    pub api_key: Option<String>,
}

impl Default for ConnectConfig {
    fn default() -> Self {
        Self {
            provider: ConnectProvider::OpenAi,
            mode: ConnectMode::CodexLogin,
            model: None,
            api_key: None,
        }
    }
}

pub fn load(paths: &AgentPaths) -> Result<ConnectConfig> {
    if !paths.connect_file.exists() {
        return Ok(ConnectConfig::default());
    }

    let raw = fs::read_to_string(&paths.connect_file)
        .with_context(|| format!("读取连接配置失败: {}", paths.connect_file.display()))?;
    let cfg: ConnectConfig = serde_json::from_str(&raw)
        .with_context(|| format!("解析连接配置失败: {}", paths.connect_file.display()))?;
    Ok(cfg)
}

pub fn save(paths: &AgentPaths, config: &ConnectConfig) -> Result<()> {
    let raw = serde_json::to_string_pretty(config)?;
    fs::write(&paths.connect_file, format!("{raw}\n"))
        .with_context(|| format!("写入连接配置失败: {}", paths.connect_file.display()))?;
    Ok(())
}

pub fn set_login(paths: &AgentPaths, model: Option<String>) -> Result<ConnectConfig> {
    let mut cfg = load(paths).unwrap_or_default();
    cfg.provider = ConnectProvider::OpenAi;
    cfg.mode = ConnectMode::CodexLogin;
    cfg.model = model;
    save(paths, &cfg)?;
    Ok(cfg)
}

pub fn set_provider_api(
    paths: &AgentPaths,
    provider: ConnectProvider,
    api_key: String,
    model: Option<String>,
) -> Result<ConnectConfig> {
    validate_api_key(&provider, &api_key)?;
    let mut cfg = load(paths).unwrap_or_default();
    cfg.provider = provider.clone();
    cfg.mode = ConnectMode::OpenAIApi;
    cfg.api_key = Some(api_key);
    if model.is_some() {
        cfg.model = model;
    } else if cfg.model.is_none() {
        cfg.model = Some(default_model_for_provider(&provider).to_string());
    }
    save(paths, &cfg)?;
    Ok(cfg)
}

pub fn set_model(paths: &AgentPaths, model: Option<String>) -> Result<ConnectConfig> {
    let mut cfg = load(paths).unwrap_or_default();
    cfg.model = model;
    save(paths, &cfg)?;
    Ok(cfg)
}

pub fn default_model_for_provider(provider: &ConnectProvider) -> &'static str {
    match provider {
        ConnectProvider::OpenAi => "gpt-4.1-mini",
        ConnectProvider::Anthropic => "claude-3-5-sonnet-latest",
        ConnectProvider::Zhipu => "glm-4-flash",
    }
}

pub fn provider_label(provider: &ConnectProvider) -> &'static str {
    match provider {
        ConnectProvider::OpenAi => "OpenAI",
        ConnectProvider::Anthropic => "Anthropic",
        ConnectProvider::Zhipu => "智谱",
    }
}

pub fn mode_label(mode: &ConnectMode) -> &'static str {
    match mode {
        ConnectMode::CodexLogin => "登录态(Codex)",
        ConnectMode::OpenAIApi => "API Key",
    }
}

pub fn account_label(cfg: &ConnectConfig) -> String {
    match cfg.mode {
        ConnectMode::CodexLogin => match codex_login_status() {
            Some(status) => status,
            None => "未知（可运行 `codex login status` 检查）".to_string(),
        },
        ConnectMode::OpenAIApi => {
            let env_var = provider_env_var(&cfg.provider);
            effective_api_key(cfg)
                .as_ref()
                .map(|key| format!("API Key({env_var}): {}", mask_api_key(key)))
                .unwrap_or_else(|| format!("API Key 未配置（{env_var}）"))
        }
    }
}

pub fn effective_api_key(cfg: &ConnectConfig) -> Option<String> {
    cfg.api_key
        .as_ref()
        .cloned()
        .or_else(|| env::var(provider_env_var(&cfg.provider)).ok())
}

pub fn validate_api_key(provider: &ConnectProvider, api_key: &str) -> Result<()> {
    let key = api_key.trim();
    if key.is_empty() {
        bail!("API Key 不能为空");
    }
    if looks_like_model_name(key) {
        bail!("你输入的更像模型名，不是 API Key");
    }

    match provider {
        ConnectProvider::OpenAi => {
            if !key.starts_with("sk-") {
                bail!("OpenAI API Key 通常以 `sk-` 开头");
            }
            if key.len() < 20 {
                bail!("OpenAI API Key 长度过短");
            }
        }
        ConnectProvider::Anthropic => {
            if !key.starts_with("sk-") {
                bail!("Anthropic API Key 通常以 `sk-` 开头");
            }
            if key.len() < 20 {
                bail!("Anthropic API Key 长度过短");
            }
        }
        ConnectProvider::Zhipu => {
            if key.len() < 16 {
                bail!("智谱 API Key 长度过短");
            }
        }
    }
    Ok(())
}

fn looks_like_model_name(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    lower.starts_with("gpt-")
        || lower.starts_with("glm-")
        || lower.starts_with("claude-")
        || lower.contains("-mini")
        || lower.contains("-flash")
        || lower.contains("-sonnet")
}

pub fn provider_env_var(provider: &ConnectProvider) -> &'static str {
    match provider {
        ConnectProvider::OpenAi => "OPENAI_API_KEY",
        ConnectProvider::Anthropic => "ANTHROPIC_API_KEY",
        ConnectProvider::Zhipu => "ZHIPU_API_KEY",
    }
}

pub fn codex_login_status() -> Option<String> {
    let output = Command::new("codex")
        .arg("login")
        .arg("status")
        .output()
        .ok()?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let raw = if stdout.trim().is_empty() {
        stderr
    } else {
        stdout
    };

    let cleaned = raw
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter(|line| !line.starts_with("WARNING:"))
        .collect::<Vec<_>>()
        .join(" / ");

    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

fn mask_api_key(key: &str) -> String {
    let visible = 4usize;
    if key.len() <= visible * 2 {
        return "****".to_string();
    }
    let head = &key[..visible];
    let tail = &key[key.len() - visible..];
    format!("{head}****{tail}")
}

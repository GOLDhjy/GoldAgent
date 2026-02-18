use crate::config::AgentPaths;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ZhipuApiType {
    General,
    #[default]
    Coding,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectConfig {
    #[serde(default)]
    pub provider: ConnectProvider,
    pub mode: ConnectMode,
    pub model: Option<String>,
    #[serde(default, alias = "openai_api_key")]
    pub api_key: Option<String>,
    #[serde(default)]
    pub zhipu_api_type: ZhipuApiType,
}

impl Default for ConnectConfig {
    fn default() -> Self {
        Self {
            provider: ConnectProvider::OpenAi,
            mode: ConnectMode::CodexLogin,
            model: None,
            api_key: None,
            zhipu_api_type: ZhipuApiType::Coding,
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
    cfg.model = Some(model.unwrap_or_else(|| "gpt-5.3-codex".to_string()));
    save(paths, &cfg)?;
    Ok(cfg)
}

pub fn set_provider_api(
    paths: &AgentPaths,
    provider: ConnectProvider,
    api_key: String,
    model: Option<String>,
    zhipu_api_type: Option<ZhipuApiType>,
) -> Result<ConnectConfig> {
    validate_api_key(&provider, &api_key)?;
    let mut cfg = load(paths).unwrap_or_default();
    let provider_changed = cfg.provider != provider;
    cfg.provider = provider.clone();
    cfg.mode = ConnectMode::OpenAIApi;
    cfg.api_key = Some(api_key);
    if matches!(provider, ConnectProvider::Zhipu) {
        cfg.zhipu_api_type = zhipu_api_type.unwrap_or_else(|| {
            if provider_changed {
                ZhipuApiType::Coding
            } else {
                cfg.zhipu_api_type
            }
        });
    }
    if let Some(model) = model {
        cfg.model = Some(normalize_model_for_provider(&provider, &model));
    } else if provider_changed || cfg.model.is_none() {
        cfg.model = Some(default_model_for_provider(&provider).to_string());
    }
    save(paths, &cfg)?;
    Ok(cfg)
}

pub fn set_model(paths: &AgentPaths, model: Option<String>) -> Result<ConnectConfig> {
    let mut cfg = load(paths).unwrap_or_default();
    cfg.model = model.map(|m| normalize_model_for_provider(&cfg.provider, &m));
    save(paths, &cfg)?;
    Ok(cfg)
}

pub fn default_model_for_provider(provider: &ConnectProvider) -> &'static str {
    match provider {
        ConnectProvider::OpenAi => "gpt-5.2",
        ConnectProvider::Anthropic => "claude-sonnet-4-5",
        ConnectProvider::Zhipu => "glm-5",
    }
}

pub fn normalize_model_for_provider(provider: &ConnectProvider, model: &str) -> String {
    let trimmed = model.trim();
    if trimmed.is_empty() {
        return trimmed.to_string();
    }

    match provider {
        ConnectProvider::OpenAi => normalize_openai_model(trimmed),
        ConnectProvider::Anthropic => normalize_anthropic_model(trimmed),
        ConnectProvider::Zhipu => {
            let lower = trimmed.to_ascii_lowercase();
            match lower.as_str() {
                "glm-5.0" | "glm5" | "glm5.0" => "glm-5".to_string(),
                "glm4.7" => "glm-4.7".to_string(),
                "glm4.7-flash" => "glm-4.7-flash".to_string(),
                "glm4.7-flashx" => "glm-4.7-flashx".to_string(),
                _ => trimmed.to_string(),
            }
        }
    }
}

fn normalize_openai_model(trimmed: &str) -> String {
    let lower = trimmed.to_ascii_lowercase();
    if let Some(effort) = parse_openai_codex_effort(&lower) {
        return format!("gpt-5.2-codex@{effort}");
    }
    match lower.as_str() {
        "gpt5.2" | "gpt-5-2" => "gpt-5.2".to_string(),
        "gpt5" | "gpt5.0" | "gpt-5.0" => "gpt-5".to_string(),
        "gpt5-mini" | "gpt5mini" | "gpt-5mini" => "gpt-5-mini".to_string(),
        "gpt5-nano" | "gpt5nano" | "gpt-5nano" => "gpt-5-nano".to_string(),
        "gpt-5-codex" | "gpt5-codex" | "gpt5.2-codex" => "gpt-5.2-codex".to_string(),
        _ => trimmed.to_string(),
    }
}

fn parse_openai_codex_effort(lower: &str) -> Option<&'static str> {
    let normalized = lower.replace('_', "-");
    let parts = normalized.split_whitespace().collect::<Vec<_>>();
    if parts.len() == 2 && is_openai_codex_alias(parts[0]) {
        return normalize_codex_effort_token(parts[1]);
    }
    if let Some((base, effort)) = normalized.rsplit_once('@')
        && is_openai_codex_alias(base)
    {
        return normalize_codex_effort_token(effort);
    }
    if let Some((base, effort)) = normalized.rsplit_once(':')
        && is_openai_codex_alias(base)
    {
        return normalize_codex_effort_token(effort);
    }
    if let Some((base, effort)) = normalized.rsplit_once('/')
        && is_openai_codex_alias(base)
    {
        return normalize_codex_effort_token(effort);
    }
    for prefix in [
        "gpt-5.2-codex-",
        "gpt-5-codex-",
        "gpt5.2-codex-",
        "gpt5-codex-",
    ] {
        if let Some(raw_effort) = normalized.strip_prefix(prefix) {
            return normalize_codex_effort_token(raw_effort);
        }
    }
    None
}

fn normalize_codex_effort_token(token: &str) -> Option<&'static str> {
    match token.trim().to_ascii_lowercase().replace('_', "-").as_str() {
        "low" => Some("low"),
        "medium" | "med" => Some("medium"),
        "high" => Some("high"),
        "xhigh" | "x-high" | "very-high" => Some("xhigh"),
        _ => None,
    }
}

fn is_openai_codex_alias(model: &str) -> bool {
    matches!(
        model.trim(),
        "gpt-5.2-codex" | "gpt-5-codex" | "gpt5.2-codex" | "gpt5-codex"
    )
}

fn normalize_anthropic_model(trimmed: &str) -> String {
    let lower = trimmed.to_ascii_lowercase();
    match lower.as_str() {
        "claude-opus-4.6" | "claude-opus-4-6-latest" => "claude-opus-4-6".to_string(),
        "claude-sonnet-4.5" | "claude-sonnet-4-5-latest" => "claude-sonnet-4-5".to_string(),
        "claude-haiku-4.5" | "claude-haiku-4-5-latest" => "claude-haiku-4-5".to_string(),
        "claude-sonnet-4.0" | "claude-sonnet-4" => "claude-sonnet-4".to_string(),
        "claude-opus-4.1" | "claude-opus-4-1-latest" => "claude-opus-4-1".to_string(),
        "claude-opus-4.0" | "claude-opus-4" => "claude-opus-4".to_string(),
        _ => trimmed.to_string(),
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
        || lower.contains("-codex")
        || lower.contains("-mini")
        || lower.contains("-nano")
        || lower.contains("-flash")
        || lower.contains("-sonnet")
        || lower.contains("-haiku")
        || lower.contains("-opus")
}

pub fn provider_env_var(provider: &ConnectProvider) -> &'static str {
    match provider {
        ConnectProvider::OpenAi => "OPENAI_API_KEY",
        ConnectProvider::Anthropic => "ANTHROPIC_API_KEY",
        ConnectProvider::Zhipu => "ZHIPU_API_KEY",
    }
}

pub fn zhipu_api_type_label(kind: ZhipuApiType) -> &'static str {
    match kind {
        ZhipuApiType::General => "普通 API",
        ZhipuApiType::Coding => "Coding Plan API",
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

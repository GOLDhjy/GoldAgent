use crate::cli::ConnectCommand;
use crate::config::AgentPaths;
use crate::connect::{self, ConnectMode, ConnectProvider, ZhipuApiType};
use crate::usage::{self, UsageEvent};
use anyhow::{Context, Result, anyhow, bail};
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::io;
use std::path::PathBuf;
use tokio::process::Command;
use uuid::Uuid;

const ZHIPU_GENERAL_CHAT_ENDPOINT: &str = "https://open.bigmodel.cn/api/paas/v4/chat/completions";
const ZHIPU_CODING_CHAT_ENDPOINT: &str =
    "https://open.bigmodel.cn/api/coding/paas/v4/chat/completions";
const OPENAI_CODEX_BASE_MODEL: &str = "gpt-5.2-codex";
const OPENAI_CODEX_TIER_MODELS: [&str; 4] = [
    "gpt-5.2-codex@low",
    "gpt-5.2-codex@medium",
    "gpt-5.2-codex@high",
    "gpt-5.2-codex@xhigh",
];

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
pub struct ProviderClient {
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
        zhipu_api_type: Option<ZhipuApiType>,
    },
    CodexExec {
        model: Option<String>,
    },
}

impl ProviderClient {
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
                let zhipu_api_type = cfg.zhipu_api_type;
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
                        return Self::build_api_backend(
                            api_key,
                            provider,
                            model,
                            usage_file,
                            Some(zhipu_api_type),
                        );
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
                let direct_model = model.unwrap_or_else(|| "gpt-5.2".to_string());
                return Self::build_api_backend(
                    &api_key,
                    ConnectProvider::OpenAi,
                    direct_model,
                    usage_file,
                    None,
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
                ..
            } => {
                let output = match provider {
                    ConnectProvider::Anthropic => {
                        chat_via_anthropic_api(http, endpoint, model, messages).await?
                    }
                    ConnectProvider::OpenAi | ConnectProvider::Zhipu => {
                        let (resolved_model, reasoning_effort) =
                            resolve_openai_compatible_model(provider, model);
                        chat_via_openai_compatible_api(
                            http,
                            endpoint,
                            &resolved_model,
                            messages,
                            reasoning_effort,
                        )
                        .await?
                    }
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
                provider,
                model,
                zhipu_api_type,
                ..
            } => {
                if matches!(provider, ConnectProvider::Zhipu) {
                    let kind = zhipu_api_type.unwrap_or(ZhipuApiType::General);
                    format!(
                        "{} / API({}) / {model}",
                        connect::provider_label(provider),
                        connect::zhipu_api_type_label(kind)
                    )
                } else {
                    format!("{} / API / {model}", connect::provider_label(provider))
                }
            }
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
        zhipu_api_type: Option<ZhipuApiType>,
    ) -> Result<Self> {
        let endpoint = api_endpoint_for_provider(&provider, zhipu_api_type)?;
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
        let zhipu_api_type = if matches!(provider, ConnectProvider::Zhipu) {
            Some(zhipu_api_type.unwrap_or(ZhipuApiType::General))
        } else {
            None
        };
        Ok(Self {
            backend: ModelBackend::ApiCompatible {
                http,
                model,
                endpoint,
                provider,
                zhipu_api_type,
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

#[derive(Clone)]
pub struct HintItem {
    pub label: String,
    pub desc: String,
    pub completion: String,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ChatCommandOutcome {
    pub handled: bool,
    pub client_changed: bool,
}

pub type PromptLineFn = fn(&str) -> io::Result<String>;

pub fn handle_connect_command(paths: &AgentPaths, command: ConnectCommand) -> Result<()> {
    match command {
        ConnectCommand::Status => {
            print_connect_status(paths)?;
        }
        ConnectCommand::Login { model } => {
            connect::set_login(paths, model)?;
            let client = ProviderClient::from_paths(paths, None)?;
            println!("已切换连接方式：{}", client.backend_label());
        }
        ConnectCommand::Api {
            api_key,
            provider,
            zhipu_api_type,
            model,
        } => {
            let provider = parse_provider_name(&provider)?;
            let zhipu_api_type = parse_zhipu_api_type_for_cli(&provider, zhipu_api_type)?;
            connect::set_provider_api(paths, provider, api_key, model, zhipu_api_type)?;
            let client = ProviderClient::from_paths(paths, None)?;
            println!("已切换连接方式：{}", client.backend_label());
        }
    }
    Ok(())
}

pub fn parse_provider_name(name: &str) -> Result<ConnectProvider> {
    match name.trim().to_ascii_lowercase().as_str() {
        "openai" => Ok(ConnectProvider::OpenAi),
        "zhipu" | "glm" => Ok(ConnectProvider::Zhipu),
        "anthropic" | "claude" => Ok(ConnectProvider::Anthropic),
        other => bail!("不支持的 provider: {other}。可选: openai, zhipu, anthropic"),
    }
}

pub fn print_connect_help(paths: &AgentPaths) -> Result<()> {
    println!("连接分类：");
    println!("- /connect openai");
    println!("- /connect anthropic");
    println!("- /connect zhipu");
    println!("统一用法：");
    println!("- /connect <provider>           先选连接方式（api/login）");
    println!("- /connect openai|anthropic api       进入 API Key 输入流程");
    println!("- /connect openai|anthropic api <KEY> [model]");
    println!("- /connect zhipu api-general [<KEY> [model]]");
    println!("- /connect zhipu api-coding [<KEY> [model]]");
    println!("- /connect openai login [model] 仅 OpenAI 支持登录态");
    println!("通用：");
    println!("- /connect status");
    print_connect_status(paths)?;
    Ok(())
}

pub fn print_model_overview(paths: &AgentPaths) -> Result<()> {
    let cfg = connect::load(paths)?;
    let current = cfg
        .model
        .as_deref()
        .unwrap_or(connect::default_model_for_provider(&cfg.provider))
        .to_string();
    let mut models = suggested_models(&cfg.provider)
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
    if matches!(cfg.provider, ConnectProvider::OpenAi)
        && matches!(cfg.mode, connect::ConnectMode::OpenAIApi)
    {
        for codex_tier in OPENAI_CODEX_TIER_MODELS {
            if !models.iter().any(|m| m == codex_tier) {
                models.push(codex_tier.to_string());
            }
        }
    }
    if !models.iter().any(|m| m == &current) {
        models.insert(0, current.clone());
    }

    println!("当前模型状态：");
    println!("- 厂商: {}", connect::provider_label(&cfg.provider));
    println!("- 当前模型: {current}");
    println!("可选模型（上下选择可补全）：");
    for model in models {
        if model == current {
            println!("- {model}  [当前]");
        } else {
            println!("- {model}");
        }
    }
    if matches!(cfg.provider, ConnectProvider::OpenAi)
        && matches!(cfg.mode, connect::ConnectMode::OpenAIApi)
    {
        println!(
            "- Codex 分档: `gpt-5.2-codex@low|medium|high|xhigh`（使用官方 reasoning effort）"
        );
    }
    println!("- 说明: 列表是内置推荐，若新版本未收录可直接输入 `/model <模型名>`。");
    Ok(())
}

pub fn print_connect_status(paths: &AgentPaths) -> Result<()> {
    let cfg = connect::load(paths)?;
    let client = ProviderClient::from_paths(paths, None)?;
    let usage_stats = usage::load(&paths.usage_file).unwrap_or_default();
    let today_key = chrono::Local::now().format("%Y-%m-%d").to_string();
    let today = usage_stats
        .by_day
        .get(&today_key)
        .cloned()
        .unwrap_or_default();
    let current_model_key = client.usage_model_key();
    let current_model_usage = usage_stats
        .by_model
        .get(&current_model_key)
        .cloned()
        .unwrap_or_default();

    println!("当前连接状态：");
    println!("- 厂商: {}", connect::provider_label(&cfg.provider));
    println!("- 模式: {}", connect::mode_label(&cfg.mode));
    println!("- 生效后端: {}", client.backend_label());
    if matches!(cfg.provider, ConnectProvider::Zhipu)
        && matches!(cfg.mode, connect::ConnectMode::OpenAIApi)
    {
        println!(
            "- 智谱 API 类型: {}",
            connect::zhipu_api_type_label(cfg.zhipu_api_type)
        );
    }
    println!(
        "- 配置模型: {}",
        cfg.model.as_deref().unwrap_or("默认模型（由后端决定）")
    );
    println!("- 账户信息: {}", connect::account_label(&cfg));
    if matches!(cfg.mode, connect::ConnectMode::OpenAIApi) {
        match connect::effective_api_key(&cfg) {
            Some(key) => {
                if let Err(err) = connect::validate_api_key(&cfg.provider, &key) {
                    println!("- 警告: 当前 API Key 可能无效：{err}");
                }
            }
            None => {
                println!("- 警告: 当前为 API 模式但未配置 API Key");
            }
        }
    }
    println!(
        "- 用量累计: 请求 {} 次, 输入 {} tokens, 输出 {} tokens",
        usage_stats.total.requests, usage_stats.total.input_tokens, usage_stats.total.output_tokens
    );
    println!(
        "- 用量今日({}): 请求 {} 次, 输入 {} tokens, 输出 {} tokens",
        today_key, today.requests, today.input_tokens, today.output_tokens
    );
    println!(
        "- 当前模型用量({}): 请求 {} 次, 输入 {} tokens, 输出 {} tokens",
        current_model_key,
        current_model_usage.requests,
        current_model_usage.input_tokens,
        current_model_usage.output_tokens
    );
    if matches!(cfg.mode, connect::ConnectMode::CodexLogin) {
        println!("- 说明: 登录态模式暂无法获取官方 token 用量，tokens 仅在 API 模式下统计。");
    }
    Ok(())
}

pub fn suggested_models(provider: &ConnectProvider) -> Vec<&'static str> {
    match provider {
        ConnectProvider::OpenAi => vec![
            "gpt-5.2",
            "gpt-5.2-codex",
        ],
        ConnectProvider::Anthropic => vec![
            "claude-opus-4-6",
            "claude-sonnet-4-5",
            "claude-haiku-4-5",
        ],
        ConnectProvider::Zhipu => vec!["glm-5", "glm-4.7", "glm-4.7-flash"],
    }
}

pub fn connect_hint_items(rest: &str) -> Vec<HintItem> {
    let trimmed = rest.trim();
    let top_level = [
        ("openai", "OpenAI（login/api）", "/connect openai "),
        (
            "anthropic",
            "Anthropic（Claude，api）",
            "/connect anthropic ",
        ),
        (
            "zhipu",
            "智谱 GLM（api-general/api-coding）",
            "/connect zhipu ",
        ),
        ("status", "查看连接/模型/账户/用量", "/connect status"),
    ];

    if trimmed.is_empty() {
        return top_level
            .iter()
            .map(|(name, desc, completion)| HintItem {
                label: (*name).to_string(),
                desc: (*desc).to_string(),
                completion: (*completion).to_string(),
            })
            .collect();
    }

    if trimmed == "status" {
        return vec![HintItem {
            label: "status".to_string(),
            desc: "回车查看连接/模型/账户/用量".to_string(),
            completion: "/connect status".to_string(),
        }];
    }

    if let Ok(provider) = parse_provider_name(trimmed) {
        return connect_methods_for_provider(&provider)
            .iter()
            .map(|method| {
                let completion = match *method {
                    "login" => format!("/connect {} login", provider_command_name(&provider)),
                    "api" => format!("/connect {} api ", provider_command_name(&provider)),
                    "api-general" => {
                        format!("/connect {} api-general ", provider_command_name(&provider))
                    }
                    "api-coding" => {
                        format!("/connect {} api-coding ", provider_command_name(&provider))
                    }
                    _ => format!("/connect {} ", provider_command_name(&provider)),
                };
                let desc = match *method {
                    "login" => "使用登录态（仅 OpenAI）",
                    "api" => "使用 API Key",
                    "api-general" => "普通 API（/api/paas）",
                    "api-coding" => "Coding Plan API（/api/coding/paas）",
                    _ => "",
                };
                HintItem {
                    label: (*method).to_string(),
                    desc: desc.to_string(),
                    completion,
                }
            })
            .collect();
    }

    if !trimmed.contains(' ') {
        let mut items = top_level
            .iter()
            .filter(|(name, _, _)| name.starts_with(trimmed))
            .map(|(name, desc, completion)| HintItem {
                label: (*name).to_string(),
                desc: (*desc).to_string(),
                completion: (*completion).to_string(),
            })
            .collect::<Vec<_>>();
        if items.is_empty() {
            items.push(HintItem {
                label: "未匹配到 connect 子命令".to_string(),
                desc: "可选: openai / anthropic / zhipu / status".to_string(),
                completion: "/connect ".to_string(),
            });
        }
        return items;
    }

    let tokens = trimmed.split_whitespace().collect::<Vec<_>>();
    let provider = match tokens
        .first()
        .and_then(|name| parse_provider_name(name).ok())
    {
        Some(provider) => provider,
        None => {
            return vec![HintItem {
                label: "connect".to_string(),
                desc: "可选: openai / anthropic / zhipu / status".to_string(),
                completion: "/connect ".to_string(),
            }];
        }
    };
    let provider_cmd = provider_command_name(&provider);
    let methods = connect_methods_for_provider(&provider);
    let method_token = tokens.get(1).copied().unwrap_or_default();

    if matches!(provider, ConnectProvider::Zhipu) && method_token == "api" {
        return vec![
            HintItem {
                label: "api-general".to_string(),
                desc: "普通 API（/api/paas）".to_string(),
                completion: format!("/connect {provider_cmd} api-general "),
            },
            HintItem {
                label: "api-coding".to_string(),
                desc: "Coding Plan API（/api/coding/paas）".to_string(),
                completion: format!("/connect {provider_cmd} api-coding "),
            },
        ];
    }

    if tokens.len() == 2 && !methods.iter().any(|m| *m == method_token) {
        let mut items = methods
            .iter()
            .filter(|method| method.starts_with(method_token))
            .map(|method| {
                let completion = match *method {
                    "login" => format!("/connect {provider_cmd} login"),
                    "api" => format!("/connect {provider_cmd} api "),
                    "api-general" => format!("/connect {provider_cmd} api-general "),
                    "api-coding" => format!("/connect {provider_cmd} api-coding "),
                    _ => format!("/connect {provider_cmd} "),
                };
                let desc = match *method {
                    "login" => "使用登录态（仅 OpenAI）",
                    "api" => "使用 API Key",
                    "api-general" => "普通 API（/api/paas）",
                    "api-coding" => "Coding Plan API（/api/coding/paas）",
                    _ => "",
                };
                HintItem {
                    label: (*method).to_string(),
                    desc: desc.to_string(),
                    completion,
                }
            })
            .collect::<Vec<_>>();
        if items.is_empty() {
            items.push(HintItem {
                label: provider_cmd.to_string(),
                desc: format!("可选方式: {}", methods.join(" / ")),
                completion: format!("/connect {provider_cmd} "),
            });
        }
        return items;
    }

    match method_token {
        "login" => {
            if !matches!(provider, ConnectProvider::OpenAi) {
                return vec![HintItem {
                    label: "api".to_string(),
                    desc: "该厂商仅支持 API Key".to_string(),
                    completion: format!("/connect {provider_cmd} api "),
                }];
            }

            if tokens.len() <= 2 {
                let mut items = vec![HintItem {
                    label: "执行切换".to_string(),
                    desc: "回车切换到 OpenAI 登录态".to_string(),
                    completion: format!("/connect {provider_cmd} login"),
                }];
                for model in suggested_models(&ConnectProvider::OpenAi) {
                    items.push(HintItem {
                        label: model.to_string(),
                        desc: "登录态指定模型".to_string(),
                        completion: format!("/connect {provider_cmd} login {model}"),
                    });
                }
                return items;
            }

            let model_prefix = tokens.get(2).copied().unwrap_or_default();
            let mut items = vec![HintItem {
                label: "执行切换".to_string(),
                desc: "回车切换到 OpenAI 登录态".to_string(),
                completion: format!("/connect {provider_cmd} login {model_prefix}"),
            }];
            for model in suggested_models(&ConnectProvider::OpenAi) {
                if model.starts_with(model_prefix) {
                    items.push(HintItem {
                        label: model.to_string(),
                        desc: "登录态指定模型".to_string(),
                        completion: format!("/connect {provider_cmd} login {model}"),
                    });
                }
            }
            return items;
        }
        "api" => {
            if matches!(provider, ConnectProvider::Zhipu) {
                return vec![
                    HintItem {
                        label: "api-general".to_string(),
                        desc: "普通 API（/api/paas）".to_string(),
                        completion: format!("/connect {provider_cmd} api-general "),
                    },
                    HintItem {
                        label: "api-coding".to_string(),
                        desc: "Coding Plan API（/api/coding/paas）".to_string(),
                        completion: format!("/connect {provider_cmd} api-coding "),
                    },
                ];
            }
            if tokens.len() == 2 {
                return vec![HintItem {
                    label: format!("<{}>", connect::provider_env_var(&provider)),
                    desc: "粘贴 key，可选再跟 model".to_string(),
                    completion: format!("/connect {provider_cmd} api "),
                }];
            }
            let key = tokens.get(2).copied().unwrap_or_default();
            if key.is_empty() {
                return vec![HintItem {
                    label: format!("<{}>", connect::provider_env_var(&provider)),
                    desc: "粘贴 key，可选再跟 model".to_string(),
                    completion: format!("/connect {provider_cmd} api "),
                }];
            }
            let model_prefix = tokens.get(3).copied().unwrap_or_default();
            let mut items = vec![HintItem {
                label: "执行切换".to_string(),
                desc: "回车切换到 API 模式".to_string(),
                completion: format!("/connect {provider_cmd} api {key}"),
            }];
            for model in suggested_models(&provider) {
                if model.starts_with(model_prefix) {
                    items.push(HintItem {
                        label: model.to_string(),
                        desc: format!("{} 模型", connect::provider_label(&provider)),
                        completion: format!("/connect {provider_cmd} api {key} {model}"),
                    });
                }
            }
            if matches!(provider, ConnectProvider::OpenAi) {
                for model in OPENAI_CODEX_TIER_MODELS {
                    if model.starts_with(model_prefix) {
                        items.push(HintItem {
                            label: model.to_string(),
                            desc: "OpenAI Codex（推理分档）".to_string(),
                            completion: format!("/connect {provider_cmd} api {key} {model}"),
                        });
                    }
                }
            }
            return items;
        }
        "api-general" | "api-coding" | "general" | "coding" | "coding-plan" => {
            if !matches!(provider, ConnectProvider::Zhipu) {
                return vec![HintItem {
                    label: provider_cmd.to_string(),
                    desc: format!("可选方式: {}", methods.join(" / ")),
                    completion: format!("/connect {provider_cmd} "),
                }];
            }
            let kind =
                parse_zhipu_api_type_from_method(method_token).unwrap_or(ZhipuApiType::General);
            if tokens.len() == 2 {
                return vec![HintItem {
                    label: format!("<{}>", connect::provider_env_var(&provider)),
                    desc: "粘贴 key，可选再跟 model".to_string(),
                    completion: format!(
                        "/connect {provider_cmd} {} ",
                        zhipu_method_from_type(kind)
                    ),
                }];
            }
            let key = tokens.get(2).copied().unwrap_or_default();
            if key.is_empty() {
                return vec![HintItem {
                    label: format!("<{}>", connect::provider_env_var(&provider)),
                    desc: "粘贴 key，可选再跟 model".to_string(),
                    completion: format!(
                        "/connect {provider_cmd} {} ",
                        zhipu_method_from_type(kind)
                    ),
                }];
            }
            let model_prefix = tokens.get(3).copied().unwrap_or_default();
            let mut items = vec![HintItem {
                label: "执行切换".to_string(),
                desc: format!("回车切换到 {}", connect::zhipu_api_type_label(kind)),
                completion: format!(
                    "/connect {provider_cmd} {} {key}",
                    zhipu_method_from_type(kind)
                ),
            }];
            for model in suggested_models(&provider) {
                if model.starts_with(model_prefix) {
                    items.push(HintItem {
                        label: model.to_string(),
                        desc: format!("{} 模型", connect::provider_label(&provider)),
                        completion: format!(
                            "/connect {provider_cmd} {} {key} {model}",
                            zhipu_method_from_type(kind)
                        ),
                    });
                }
            }
            return items;
        }
        _ => {}
    }

    vec![HintItem {
        label: provider_cmd.to_string(),
        desc: format!("可选方式: {}", methods.join(" / ")),
        completion: format!("/connect {provider_cmd} "),
    }]
}

pub fn model_hint_items(paths: &AgentPaths, rest: &str) -> Vec<HintItem> {
    let trimmed = rest.trim();
    let cfg = match connect::load(paths) {
        Ok(cfg) => cfg,
        Err(_) => return Vec::new(),
    };
    let current = cfg
        .model
        .as_deref()
        .unwrap_or(connect::default_model_for_provider(&cfg.provider))
        .to_string();
    let mut models = suggested_models(&cfg.provider)
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
    if matches!(cfg.provider, ConnectProvider::OpenAi)
        && matches!(cfg.mode, connect::ConnectMode::OpenAIApi)
    {
        for codex_tier in OPENAI_CODEX_TIER_MODELS {
            if !models.iter().any(|m| m == codex_tier) {
                models.push(codex_tier.to_string());
            }
        }
    }
    if !models.iter().any(|m| m == &current) {
        models.insert(0, current.clone());
    }

    let mut items = models
        .iter()
        .filter(|m| trimmed.is_empty() || m.starts_with(trimmed))
        .map(|m| HintItem {
            label: m.clone(),
            desc: if *m == current {
                "当前模型".to_string()
            } else {
                "回车切换到该模型".to_string()
            },
            completion: format!("/model {m}"),
        })
        .collect::<Vec<_>>();

    if items.is_empty() && !trimmed.is_empty() {
        items.push(HintItem {
            label: trimmed.to_string(),
            desc: "自定义模型（回车切换）".to_string(),
            completion: format!("/model {trimmed}"),
        });
    }

    items
}

pub fn handle_connect_chat_command(
    paths: &AgentPaths,
    client: &mut ProviderClient,
    rest: &str,
    prompt_line: PromptLineFn,
) -> Result<ChatCommandOutcome> {
    let trimmed = rest.trim();
    if trimmed.is_empty() {
        print_connect_help(paths)?;
        return Ok(ChatCommandOutcome {
            handled: true,
            client_changed: false,
        });
    }
    if trimmed == "status" {
        print_connect_status(paths)?;
        return Ok(ChatCommandOutcome {
            handled: true,
            client_changed: false,
        });
    }

    let mut parts = trimmed.split_whitespace();
    let Some(provider_token) = parts.next() else {
        return Ok(ChatCommandOutcome::default());
    };
    let provider = match parse_provider_name(provider_token) {
        Ok(provider) => provider,
        Err(_) => return Ok(ChatCommandOutcome::default()),
    };

    let method = parts.next();
    match method {
        None => {
            print_provider_connect_methods(&provider);
            let method = prompt_line("请选择连接方式（回车取消）: ")?;
            let method = method.trim().to_ascii_lowercase();
            if method.is_empty() {
                println!("已取消连接。");
                return Ok(ChatCommandOutcome {
                    handled: true,
                    client_changed: false,
                });
            }
            match method.as_str() {
                "login" => {
                    if !matches!(provider, ConnectProvider::OpenAi) {
                        println!(
                            "{} 目前仅支持 api 方式。",
                            connect::provider_label(&provider)
                        );
                        return Ok(ChatCommandOutcome {
                            handled: true,
                            client_changed: false,
                        });
                    }
                    let model = prompt_line("请输入模型（可选，回车默认模型）: ")?;
                    let model = if model.trim().is_empty() {
                        None
                    } else {
                        Some(model.trim().to_string())
                    };
                    connect_openai_login(paths, client, model)?;
                    return Ok(ChatCommandOutcome {
                        handled: true,
                        client_changed: true,
                    });
                }
                "api" => {
                    if matches!(provider, ConnectProvider::Zhipu) {
                        println!("智谱请使用 `api-general` 或 `api-coding`。");
                        return Ok(ChatCommandOutcome {
                            handled: true,
                            client_changed: false,
                        });
                    }
                    let changed = connect_provider_api_interactive(
                        paths,
                        client,
                        provider.clone(),
                        None,
                        prompt_line,
                    )?;
                    return Ok(ChatCommandOutcome {
                        handled: true,
                        client_changed: changed,
                    });
                }
                "api-general" | "api-coding" | "general" | "coding" | "coding-plan" => {
                    if !matches!(provider, ConnectProvider::Zhipu) {
                        println!("{} 不支持该连接方式。", connect::provider_label(&provider));
                        return Ok(ChatCommandOutcome {
                            handled: true,
                            client_changed: false,
                        });
                    }
                    let kind =
                        parse_zhipu_api_type_from_method(&method).unwrap_or(ZhipuApiType::General);
                    let changed = connect_provider_api_interactive(
                        paths,
                        client,
                        provider.clone(),
                        Some(kind),
                        prompt_line,
                    )?;
                    return Ok(ChatCommandOutcome {
                        handled: true,
                        client_changed: changed,
                    });
                }
                _ => {
                    let allowed = connect_methods_for_provider(&provider).join(" / ");
                    println!("不支持的连接方式：{method}。可选：{allowed}");
                }
            }
            Ok(ChatCommandOutcome {
                handled: true,
                client_changed: false,
            })
        }
        Some("login") => {
            if !matches!(provider, ConnectProvider::OpenAi) {
                println!(
                    "{} 不支持 login，仅支持 api。",
                    connect::provider_label(&provider)
                );
                return Ok(ChatCommandOutcome {
                    handled: true,
                    client_changed: false,
                });
            }
            let model = parts.next().map(str::to_string);
            connect_openai_login(paths, client, model)?;
            Ok(ChatCommandOutcome {
                handled: true,
                client_changed: true,
            })
        }
        Some("api") => {
            if matches!(provider, ConnectProvider::Zhipu) {
                println!("智谱请使用 `api-general` 或 `api-coding`。");
                return Ok(ChatCommandOutcome {
                    handled: true,
                    client_changed: false,
                });
            }
            if let Some(api_key) = parts.next() {
                let model = parts.next().map(str::to_string);
                if let Err(err) = connect_provider_api(
                    paths,
                    client,
                    provider.clone(),
                    api_key.to_string(),
                    model,
                    None,
                ) {
                    println!("连接失败：{err}");
                    return Ok(ChatCommandOutcome {
                        handled: true,
                        client_changed: false,
                    });
                }
                return Ok(ChatCommandOutcome {
                    handled: true,
                    client_changed: true,
                });
            }
            let changed = connect_provider_api_interactive(
                paths,
                client,
                provider.clone(),
                None,
                prompt_line,
            )?;
            Ok(ChatCommandOutcome {
                handled: true,
                client_changed: changed,
            })
        }
        Some("api-general") | Some("api-coding") | Some("general") | Some("coding")
        | Some("coding-plan") => {
            if !matches!(provider, ConnectProvider::Zhipu) {
                println!("{} 不支持该连接方式。", connect::provider_label(&provider));
                return Ok(ChatCommandOutcome {
                    handled: true,
                    client_changed: false,
                });
            }
            let kind = parse_zhipu_api_type_from_method(method.unwrap_or("api-general"))
                .unwrap_or(ZhipuApiType::General);
            if let Some(api_key) = parts.next() {
                let model = parts.next().map(str::to_string);
                if let Err(err) = connect_provider_api(
                    paths,
                    client,
                    provider.clone(),
                    api_key.to_string(),
                    model,
                    Some(kind),
                ) {
                    println!("连接失败：{err}");
                    return Ok(ChatCommandOutcome {
                        handled: true,
                        client_changed: false,
                    });
                }
                return Ok(ChatCommandOutcome {
                    handled: true,
                    client_changed: true,
                });
            }
            let changed = connect_provider_api_interactive(
                paths,
                client,
                provider.clone(),
                Some(kind),
                prompt_line,
            )?;
            Ok(ChatCommandOutcome {
                handled: true,
                client_changed: changed,
            })
        }
        Some(other) => {
            let allowed = connect_methods_for_provider(&provider).join(" / ");
            println!(
                "{} 不支持连接方式：{other}。可选：{allowed}",
                provider_command_name(&provider)
            );
            Ok(ChatCommandOutcome {
                handled: true,
                client_changed: false,
            })
        }
    }
}

pub fn handle_model_chat_command(
    paths: &AgentPaths,
    client: &mut ProviderClient,
    input: &str,
) -> Result<ChatCommandOutcome> {
    if input == "/model" || input == "/model " || input == "/model status" || input == "/model list"
    {
        print_model_overview(paths)?;
        return Ok(ChatCommandOutcome {
            handled: true,
            client_changed: false,
        });
    }

    if let Some(rest) = input.strip_prefix("/model set ") {
        let model = rest.trim();
        if model.is_empty() {
            println!("用法：/model set <model>");
            print_model_overview(paths)?;
            return Ok(ChatCommandOutcome {
                handled: true,
                client_changed: false,
            });
        }
        connect::set_model(paths, Some(model.to_string()))?;
        *client = ProviderClient::from_paths(paths, None)?;
        println!("已切换模型：{}", client.backend_label());
        return Ok(ChatCommandOutcome {
            handled: true,
            client_changed: true,
        });
    }

    if let Some(rest) = input.strip_prefix("/model ") {
        let model = rest.trim();
        if model.is_empty() {
            print_model_overview(paths)?;
            return Ok(ChatCommandOutcome {
                handled: true,
                client_changed: false,
            });
        }
        if model == "status" || model == "list" {
            print_model_overview(paths)?;
            return Ok(ChatCommandOutcome {
                handled: true,
                client_changed: false,
            });
        }
        if let Some(raw) = model.strip_prefix("set ") {
            let target = raw.trim();
            if target.is_empty() {
                println!("用法：/model <model>");
                print_model_overview(paths)?;
                return Ok(ChatCommandOutcome {
                    handled: true,
                    client_changed: false,
                });
            }
            connect::set_model(paths, Some(target.to_string()))?;
            *client = ProviderClient::from_paths(paths, None)?;
            println!("已切换模型：{}", client.backend_label());
            return Ok(ChatCommandOutcome {
                handled: true,
                client_changed: true,
            });
        }
        connect::set_model(paths, Some(model.to_string()))?;
        *client = ProviderClient::from_paths(paths, None)?;
        println!("已切换模型：{}", client.backend_label());
        return Ok(ChatCommandOutcome {
            handled: true,
            client_changed: true,
        });
    }

    Ok(ChatCommandOutcome::default())
}

fn provider_command_name(provider: &ConnectProvider) -> &'static str {
    match provider {
        ConnectProvider::OpenAi => "openai",
        ConnectProvider::Anthropic => "anthropic",
        ConnectProvider::Zhipu => "zhipu",
    }
}

fn connect_methods_for_provider(provider: &ConnectProvider) -> &'static [&'static str] {
    match provider {
        ConnectProvider::OpenAi => &["login", "api"],
        ConnectProvider::Anthropic => &["api"],
        ConnectProvider::Zhipu => &["api-general", "api-coding"],
    }
}

fn print_provider_connect_methods(provider: &ConnectProvider) {
    println!("{} 连接方式：", connect::provider_label(provider));
    for method in connect_methods_for_provider(provider) {
        match *method {
            "login" => println!("- login（登录态）"),
            "api" => println!("- api（API Key）"),
            "api-general" => println!("- api-general（普通 API）"),
            "api-coding" => println!("- api-coding（Coding Plan API）"),
            _ => {}
        }
    }
}

fn connect_openai_login(
    paths: &AgentPaths,
    client: &mut ProviderClient,
    model: Option<String>,
) -> Result<()> {
    connect::set_login(paths, model)?;
    *client = ProviderClient::from_paths(paths, None)?;
    println!("已切换连接方式：{}", client.backend_label());
    Ok(())
}

fn connect_provider_api(
    paths: &AgentPaths,
    client: &mut ProviderClient,
    provider: ConnectProvider,
    api_key: String,
    model: Option<String>,
    zhipu_api_type: Option<ZhipuApiType>,
) -> Result<()> {
    connect::set_provider_api(paths, provider, api_key, model, zhipu_api_type)?;
    *client = ProviderClient::from_paths(paths, None)?;
    println!("已切换连接方式：{}", client.backend_label());
    Ok(())
}

fn connect_provider_api_interactive(
    paths: &AgentPaths,
    client: &mut ProviderClient,
    provider: ConnectProvider,
    zhipu_api_type: Option<ZhipuApiType>,
    prompt_line: PromptLineFn,
) -> Result<bool> {
    let env_var = connect::provider_env_var(&provider);
    let api_key = prompt_line(&format!("请输入 {env_var}（留空取消）: "))?;
    let api_key = api_key.trim().to_string();
    if api_key.is_empty() {
        println!("已取消连接。");
        return Ok(false);
    }

    let model = prompt_line(&format!(
        "请输入模型（可选，回车默认 {}）: ",
        connect::default_model_for_provider(&provider)
    ))?;
    let model = if model.trim().is_empty() {
        None
    } else {
        Some(model.trim().to_string())
    };

    if let Err(err) = connect_provider_api(paths, client, provider, api_key, model, zhipu_api_type)
    {
        println!("连接失败：{err}");
        return Ok(false);
    }
    Ok(true)
}

fn parse_zhipu_api_type_from_method(method: &str) -> Option<ZhipuApiType> {
    match method {
        "api-general" | "general" => Some(ZhipuApiType::General),
        "api-coding" | "coding" | "coding-plan" => Some(ZhipuApiType::Coding),
        _ => None,
    }
}

fn zhipu_method_from_type(kind: ZhipuApiType) -> &'static str {
    match kind {
        ZhipuApiType::General => "api-general",
        ZhipuApiType::Coding => "api-coding",
    }
}

fn parse_zhipu_api_type_for_cli(
    provider: &ConnectProvider,
    raw: Option<String>,
) -> Result<Option<ZhipuApiType>> {
    if !matches!(provider, ConnectProvider::Zhipu) {
        if raw.as_deref().is_some_and(|v| !v.trim().is_empty()) {
            bail!("--zhipu-api-type 仅可与 --provider zhipu 一起使用");
        }
        return Ok(None);
    }
    match raw.as_deref().map(str::trim) {
        None | Some("") => Ok(None),
        Some(value) => {
            let value = value.to_ascii_lowercase();
            match value.as_str() {
                "general" | "api-general" => Ok(Some(ZhipuApiType::General)),
                "coding" | "coding-plan" | "api-coding" => Ok(Some(ZhipuApiType::Coding)),
                _ => bail!("zhipu_api_type 仅支持 general 或 coding"),
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum OpenAiReasoningEffort {
    Low,
    Medium,
    High,
    Xhigh,
}

impl OpenAiReasoningEffort {
    fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
        }
    }
}

fn resolve_openai_compatible_model(
    provider: &ConnectProvider,
    configured_model: &str,
) -> (String, Option<OpenAiReasoningEffort>) {
    if !matches!(provider, ConnectProvider::OpenAi) {
        return (configured_model.to_string(), None);
    }
    if let Some((model, effort)) = parse_openai_model_and_effort(configured_model) {
        return (model, Some(effort));
    }
    let model = configured_model.trim().to_ascii_lowercase();
    if matches!(
        model.as_str(),
        "gpt-5-codex" | "gpt5-codex" | "gpt5.2-codex"
    ) {
        return (OPENAI_CODEX_BASE_MODEL.to_string(), None);
    }
    (configured_model.to_string(), None)
}

fn parse_openai_model_and_effort(model: &str) -> Option<(String, OpenAiReasoningEffort)> {
    let normalized = model.trim().to_ascii_lowercase().replace('_', "-");
    if normalized.is_empty() {
        return None;
    }

    let parts = normalized.split_whitespace().collect::<Vec<_>>();
    if parts.len() == 2
        && is_openai_codex_model(parts[0])
        && let Some(effort) = parse_reasoning_effort(parts[1])
    {
        return Some((OPENAI_CODEX_BASE_MODEL.to_string(), effort));
    }

    if let Some((base, effort)) = normalized.rsplit_once('@')
        && is_openai_codex_model(base)
        && let Some(parsed_effort) = parse_reasoning_effort(effort)
    {
        return Some((OPENAI_CODEX_BASE_MODEL.to_string(), parsed_effort));
    }

    if let Some((base, effort)) = normalized.rsplit_once(':')
        && is_openai_codex_model(base)
        && let Some(parsed_effort) = parse_reasoning_effort(effort)
    {
        return Some((OPENAI_CODEX_BASE_MODEL.to_string(), parsed_effort));
    }

    if let Some((base, effort)) = normalized.rsplit_once('/')
        && is_openai_codex_model(base)
        && let Some(parsed_effort) = parse_reasoning_effort(effort)
    {
        return Some((OPENAI_CODEX_BASE_MODEL.to_string(), parsed_effort));
    }

    for prefix in [
        "gpt-5.2-codex-",
        "gpt-5-codex-",
        "gpt5.2-codex-",
        "gpt5-codex-",
    ] {
        if let Some(raw_effort) = normalized.strip_prefix(prefix)
            && let Some(parsed_effort) = parse_reasoning_effort(raw_effort)
        {
            return Some((OPENAI_CODEX_BASE_MODEL.to_string(), parsed_effort));
        }
    }

    None
}

fn is_openai_codex_model(model: &str) -> bool {
    matches!(
        model.trim(),
        "gpt-5.2-codex" | "gpt-5-codex" | "gpt5.2-codex" | "gpt5-codex"
    )
}

fn parse_reasoning_effort(raw: &str) -> Option<OpenAiReasoningEffort> {
    match raw.trim().to_ascii_lowercase().replace('_', "-").as_str() {
        "low" => Some(OpenAiReasoningEffort::Low),
        "medium" | "med" => Some(OpenAiReasoningEffort::Medium),
        "high" => Some(OpenAiReasoningEffort::High),
        "xhigh" | "x-high" => Some(OpenAiReasoningEffort::Xhigh),
        _ => None,
    }
}

fn codex_cli_model_name(model: &str) -> String {
    resolve_openai_compatible_model(&ConnectProvider::OpenAi, model).0
}

async fn chat_via_openai_compatible_api(
    http: &reqwest::Client,
    endpoint: &str,
    model: &str,
    messages: &[ChatMessage],
    reasoning_effort: Option<OpenAiReasoningEffort>,
) -> Result<ChatApiOutput> {
    let body = ChatCompletionRequest {
        model: model.to_string(),
        messages: messages.to_vec(),
        temperature: 0.2,
        reasoning: reasoning_effort.map(|effort| ChatReasoning {
            effort: effort.as_str().to_string(),
        }),
    };

    let response = http
        .post(endpoint)
        .json(&body)
        .send()
        .await
        .with_context(|| format!("Failed to call API: {endpoint}"))?;
    let status = response.status();
    let body_text = response.text().await.unwrap_or_default();
    let mut parsed = if status.is_success() {
        serde_json::from_str::<ChatCompletionResponse>(&body_text).with_context(|| {
            format!("Failed to parse OpenAI chat completion response: {body_text}")
        })
    } else {
        bail!("API error {status}: {body_text}");
    };
    if parsed.is_err() && reasoning_effort.is_some() {
        let lower = body_text.to_ascii_lowercase();
        if lower.contains("reasoning") || lower.contains("effort") {
            let fallback_body = ChatCompletionRequest {
                model: model.to_string(),
                messages: messages.to_vec(),
                temperature: 0.2,
                reasoning: None,
            };
            let fallback_response = http
                .post(endpoint)
                .json(&fallback_body)
                .send()
                .await
                .with_context(|| format!("Failed to call API: {endpoint}"))?;
            let status = fallback_response.status();
            let fallback_text = fallback_response.text().await.unwrap_or_default();
            parsed = if status.is_success() {
                serde_json::from_str::<ChatCompletionResponse>(&fallback_text).with_context(|| {
                    format!("Failed to parse OpenAI chat completion response: {fallback_text}")
                })
            } else {
                bail!("API error {status}: {fallback_text}");
            };
        }
    }
    let parsed = parsed?;

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

fn api_endpoint_for_provider(
    provider: &ConnectProvider,
    zhipu_api_type: Option<ZhipuApiType>,
) -> Result<String> {
    match provider {
        ConnectProvider::OpenAi => Ok("https://api.openai.com/v1/chat/completions".to_string()),
        ConnectProvider::Zhipu => match zhipu_api_type.unwrap_or(ZhipuApiType::General) {
            ZhipuApiType::General => Ok(ZHIPU_GENERAL_CHAT_ENDPOINT.to_string()),
            ZhipuApiType::Coding => Ok(ZHIPU_CODING_CHAT_ENDPOINT.to_string()),
        },
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
        cmd.arg("--model").arg(codex_cli_model_name(&model));
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
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning: Option<ChatReasoning>,
}

#[derive(Debug, Serialize)]
struct ChatReasoning {
    effort: String,
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

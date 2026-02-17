mod cli;
mod config;
mod connect;
mod jobs;
mod memory;
mod openai;
mod scheduler;
mod shell;
mod skills;
mod usage;

use anyhow::{Result, bail};
use clap::Parser;
use cli::{Cli, Commands, ConnectCommand, CronCommand, SkillCommand};
use config::AgentPaths;
use openai::{ChatMessage, OpenAIClient};
use std::cmp;
use std::io::{self, Read, Write};

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let paths = AgentPaths::new()?;
    paths.ensure()?;

    let command = cli.command.unwrap_or(Commands::Chat { model: None });

    match command {
        Commands::Init => {
            println!("GoldAgent 已初始化：{}", paths.root.display());
        }
        Commands::Chat { model } => {
            chat_loop(&paths, model).await?;
        }
        Commands::Run { task, model } => {
            run_task(&paths, &task, model).await?;
        }
        Commands::Serve => {
            scheduler::serve(paths).await?;
        }
        Commands::Shell { cmd, force } => {
            let output = shell::run_shell_command(&cmd, force).await?;
            if !output.stdout.trim().is_empty() {
                println!("{}", output.stdout.trim_end());
            }
            if !output.stderr.trim().is_empty() {
                eprintln!("{}", output.stderr.trim_end());
            }
            memory::append_short_term(&paths, "shell.manual", &format!("$ {cmd}"))?;
        }
        Commands::Connect { command } => handle_connect_command(&paths, command)?,
        Commands::Cron { command } => handle_cron_command(&paths, command)?,
        Commands::Skill { command } => handle_skill_command(&paths, command).await?,
    }

    Ok(())
}

async fn run_task(paths: &AgentPaths, task: &str, model: Option<String>) -> Result<()> {
    let client = OpenAIClient::from_paths(paths, model)?;
    let memory_context = memory::tail_context(paths, 4_000)?;
    let _ = memory::capture_explicit_remember(paths, "run.task", task)?;

    let system = format!(
        "You are GoldAgent, a local assistant.\nUse memory carefully and answer concisely.\n\nMemory context:\n{memory_context}"
    );

    let response = client
        .chat(&[ChatMessage::system(system), ChatMessage::user(task)])
        .await?;

    println!("{response}");
    memory::append_short_term(
        paths,
        "run.task",
        &format!("task:\n{task}\n\nresponse:\n{response}"),
    )?;
    memory::auto_capture_long_term(paths, "run.task", task)?;
    Ok(())
}

async fn chat_loop(paths: &AgentPaths, model: Option<String>) -> Result<()> {
    let mut client = OpenAIClient::from_paths(paths, model)?;
    let memory_context = memory::tail_context(paths, 4_000)?;

    let mut messages = vec![ChatMessage::system(format!(
        "You are GoldAgent, a local assistant.\nMemory context:\n{memory_context}"
    ))];

    print_chat_header(&client);
    print_chat_commands_hint();

    loop {
        let Some(line) = readline_with_inline_hint(paths, "you ❯ ")? else {
            break;
        };
        let input = line.trim();

        if input.is_empty() {
            continue;
        }

        if input.starts_with('/') {
            let action = handle_chat_slash(paths, &mut client, input, &mut messages).await?;
            if matches!(action, SlashAction::Exit) {
                break;
            }
            continue;
        }

        let _ = memory::capture_explicit_remember(paths, "chat.turn", input)?;
        messages.push(ChatMessage::user(input));
        let response = client.chat(&messages).await?;

        print_assistant_block(&response);
        messages.push(ChatMessage::assistant(response.clone()));

        silently_capture_before_compaction(paths, &messages)?;
        trim_history(&mut messages, 14);

        memory::append_short_term(
            paths,
            "chat.turn",
            &format!("user:\n{input}\n\nassistant:\n{response}"),
        )?;
        memory::auto_capture_long_term(paths, "chat.turn", input)?;
    }

    println!("已退出 GoldAgent 对话。");
    Ok(())
}

fn print_chat_header(client: &OpenAIClient) {
    println!();
    println!("+----------------------------------------------+");
    println!("|                GoldAgent Chat                |");
    println!("+----------------------------------------------+");
    println!("后端: {}", client.backend_label());
}

fn print_chat_commands_hint() {
    println!("输入 `/` 可查看命令。");
    println!("示例：`/model`、`/skill <skill名> <输入>`、`/connect openai`");
    println!();
}

fn print_assistant_block(response: &str) {
    println!("+ goldagent");
    for line in response.lines() {
        println!("| {line}");
    }
    println!("+----------------------------------------------");
}

enum SlashAction {
    Continue,
    Exit,
}

async fn handle_chat_slash(
    paths: &AgentPaths,
    client: &mut OpenAIClient,
    input: &str,
    messages: &mut Vec<ChatMessage>,
) -> Result<SlashAction> {
    match input {
        "/" | "/help" => {
            print_command_palette(paths)?;
            return Ok(SlashAction::Continue);
        }
        "/exit" | "/quit" => return Ok(SlashAction::Exit),
        "/clear" => {
            print!("\x1B[2J\x1B[H");
            print_chat_header(client);
            print_chat_commands_hint();
            return Ok(SlashAction::Continue);
        }
        _ => {}
    }

    if input == "/connect" || input == "/connect " {
        print_connect_help(paths)?;
        return Ok(SlashAction::Continue);
    }

    if input == "/connect status" {
        print_connect_status(paths)?;
        return Ok(SlashAction::Continue);
    }

    if let Some(rest) = input.strip_prefix("/connect ") {
        if handle_connect_chat_command(paths, client, rest)? {
            return Ok(SlashAction::Continue);
        }
    }

    if input == "/model" || input == "/model " || input == "/model status" || input == "/model list"
    {
        print_model_overview(paths)?;
        return Ok(SlashAction::Continue);
    }

    if let Some(rest) = input.strip_prefix("/model set ") {
        let model = rest.trim();
        if model.is_empty() {
            println!("用法：/model set <model>");
            print_model_overview(paths)?;
            return Ok(SlashAction::Continue);
        }
        connect::set_model(paths, Some(model.to_string()))?;
        *client = OpenAIClient::from_paths(paths, None)?;
        println!("已切换模型：{}", client.backend_label());
        return Ok(SlashAction::Continue);
    }

    if let Some(rest) = input.strip_prefix("/model ") {
        let model = rest.trim();
        if model.is_empty() {
            print_model_overview(paths)?;
            return Ok(SlashAction::Continue);
        }
        if model == "status" || model == "list" {
            print_model_overview(paths)?;
            return Ok(SlashAction::Continue);
        }
        if let Some(raw) = model.strip_prefix("set ") {
            let target = raw.trim();
            if target.is_empty() {
                println!("用法：/model <model>");
                print_model_overview(paths)?;
                return Ok(SlashAction::Continue);
            }
            connect::set_model(paths, Some(target.to_string()))?;
            *client = OpenAIClient::from_paths(paths, None)?;
            println!("已切换模型：{}", client.backend_label());
            return Ok(SlashAction::Continue);
        }
        connect::set_model(paths, Some(model.to_string()))?;
        *client = OpenAIClient::from_paths(paths, None)?;
        println!("已切换模型：{}", client.backend_label());
        return Ok(SlashAction::Continue);
    }

    if input == "/skill" || input == "/skill " {
        println!("用法：/skill <skill名> <输入内容>");
        print_skills_for_chat(paths)?;
        return Ok(SlashAction::Continue);
    }

    if let Some(rest) = input.strip_prefix("/skill ") {
        let trimmed = rest.trim();
        if trimmed.is_empty() {
            println!("用法：/skill <skill名> <输入内容>");
            print_skills_for_chat(paths)?;
            return Ok(SlashAction::Continue);
        }

        let Some((skill_name, skill_input)) = trimmed.split_once(' ') else {
            let candidates = suggest_skills(paths, trimmed)?;
            if candidates.is_empty() {
                println!("未找到匹配技能：{trimmed}");
            } else {
                println!("匹配技能：{}", candidates.join(", "));
                println!("继续输入：/skill <skill名> <输入内容>");
            }
            return Ok(SlashAction::Continue);
        };

        let response =
            run_skill_and_record(paths, client, skill_name.trim(), skill_input.trim()).await?;
        print_assistant_block(&response);

        messages.push(ChatMessage::user(format!(
            "/skill {} {}",
            skill_name.trim(),
            skill_input.trim()
        )));
        messages.push(ChatMessage::assistant(response));
        silently_capture_before_compaction(paths, messages)?;
        trim_history(messages, 14);
        return Ok(SlashAction::Continue);
    }

    let suggestions = command_suggestions(input);
    println!("未知命令：{input}");
    if suggestions.is_empty() {
        println!("输入 `/help` 查看可用命令。");
    } else {
        println!("你可能想用：{}", suggestions.join(", "));
    }

    Ok(SlashAction::Continue)
}

fn print_command_palette(paths: &AgentPaths) -> Result<()> {
    println!();
    println!("可用命令：");
    println!("- /help");
    println!("- /exit");
    println!("- /clear");
    println!("- /model");
    println!("- /connect");
    println!("- /connect status");
    println!("- /connect openai ...");
    println!("- /connect anthropic ...");
    println!("- /connect zhipu ...");
    println!("- /skill <skill名> <输入内容>");
    print_connect_status(paths)?;
    print_skills_for_chat(paths)?;
    println!();
    Ok(())
}

fn print_connect_help(paths: &AgentPaths) -> Result<()> {
    println!("连接分类：");
    println!("- /connect openai");
    println!("- /connect anthropic");
    println!("- /connect zhipu");
    println!("统一用法：");
    println!("- /connect <provider>           先选连接方式（api/login）");
    println!("- /connect <provider> api       进入 API Key 输入流程");
    println!("- /connect <provider> api <KEY> [model]");
    println!("- /connect openai login [model] 仅 OpenAI 支持登录态");
    println!("通用：");
    println!("- /connect status");
    print_connect_status(paths)?;
    Ok(())
}

fn provider_command_name(provider: &connect::ConnectProvider) -> &'static str {
    match provider {
        connect::ConnectProvider::OpenAi => "openai",
        connect::ConnectProvider::Anthropic => "anthropic",
        connect::ConnectProvider::Zhipu => "zhipu",
    }
}

fn connect_methods_for_provider(provider: &connect::ConnectProvider) -> &'static [&'static str] {
    match provider {
        connect::ConnectProvider::OpenAi => &["login", "api"],
        connect::ConnectProvider::Anthropic | connect::ConnectProvider::Zhipu => &["api"],
    }
}

fn print_provider_connect_methods(provider: &connect::ConnectProvider) {
    println!("{} 连接方式：", connect::provider_label(provider));
    for method in connect_methods_for_provider(provider) {
        match *method {
            "login" => println!("- login（登录态）"),
            "api" => println!("- api（API Key）"),
            _ => {}
        }
    }
}

fn connect_openai_login(
    paths: &AgentPaths,
    client: &mut OpenAIClient,
    model: Option<String>,
) -> Result<()> {
    connect::set_login(paths, model)?;
    *client = OpenAIClient::from_paths(paths, None)?;
    println!("已切换连接方式：{}", client.backend_label());
    Ok(())
}

fn connect_provider_api(
    paths: &AgentPaths,
    client: &mut OpenAIClient,
    provider: connect::ConnectProvider,
    api_key: String,
    model: Option<String>,
) -> Result<()> {
    connect::set_provider_api(paths, provider, api_key, model)?;
    *client = OpenAIClient::from_paths(paths, None)?;
    println!("已切换连接方式：{}", client.backend_label());
    Ok(())
}

fn connect_provider_api_interactive(
    paths: &AgentPaths,
    client: &mut OpenAIClient,
    provider: connect::ConnectProvider,
) -> Result<()> {
    let env_var = connect::provider_env_var(&provider);
    let api_key = prompt_line(&format!("请输入 {env_var}（留空取消）: "))?;
    let api_key = api_key.trim().to_string();
    if api_key.is_empty() {
        println!("已取消连接。");
        return Ok(());
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

    if let Err(err) = connect_provider_api(paths, client, provider, api_key, model) {
        println!("连接失败：{err}");
    }
    Ok(())
}

fn handle_connect_chat_command(
    paths: &AgentPaths,
    client: &mut OpenAIClient,
    rest: &str,
) -> Result<bool> {
    let trimmed = rest.trim();
    if trimmed.is_empty() {
        print_connect_help(paths)?;
        return Ok(true);
    }
    if trimmed == "status" {
        print_connect_status(paths)?;
        return Ok(true);
    }

    let mut parts = trimmed.split_whitespace();
    let Some(provider_token) = parts.next() else {
        return Ok(false);
    };
    let provider = match parse_provider_name(provider_token) {
        Ok(provider) => provider,
        Err(_) => return Ok(false),
    };

    let method = parts.next();
    match method {
        None => {
            print_provider_connect_methods(&provider);
            let method = prompt_line("请选择连接方式（回车取消）: ")?;
            let method = method.trim().to_ascii_lowercase();
            if method.is_empty() {
                println!("已取消连接。");
                return Ok(true);
            }
            match method.as_str() {
                "login" => {
                    if !matches!(provider, connect::ConnectProvider::OpenAi) {
                        println!(
                            "{} 目前仅支持 api 方式。",
                            connect::provider_label(&provider)
                        );
                        return Ok(true);
                    }
                    let model = prompt_line("请输入模型（可选，回车默认模型）: ")?;
                    let model = if model.trim().is_empty() {
                        None
                    } else {
                        Some(model.trim().to_string())
                    };
                    connect_openai_login(paths, client, model)?;
                }
                "api" => {
                    connect_provider_api_interactive(paths, client, provider.clone())?;
                }
                _ => {
                    let allowed = connect_methods_for_provider(&provider).join(" / ");
                    println!("不支持的连接方式：{method}。可选：{allowed}");
                }
            }
            Ok(true)
        }
        Some("login") => {
            if !matches!(provider, connect::ConnectProvider::OpenAi) {
                println!(
                    "{} 不支持 login，仅支持 api。",
                    connect::provider_label(&provider)
                );
                return Ok(true);
            }
            let model = parts.next().map(str::to_string);
            connect_openai_login(paths, client, model)?;
            Ok(true)
        }
        Some("api") => {
            if let Some(api_key) = parts.next() {
                let model = parts.next().map(str::to_string);
                if let Err(err) = connect_provider_api(
                    paths,
                    client,
                    provider.clone(),
                    api_key.to_string(),
                    model,
                ) {
                    println!("连接失败：{err}");
                }
                return Ok(true);
            }
            connect_provider_api_interactive(paths, client, provider.clone())?;
            Ok(true)
        }
        Some(other) => {
            let allowed = connect_methods_for_provider(&provider).join(" / ");
            println!(
                "{} 不支持连接方式：{other}。可选：{allowed}",
                provider_command_name(&provider)
            );
            Ok(true)
        }
    }
}

fn print_model_overview(paths: &AgentPaths) -> Result<()> {
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
    println!("- 说明: 列表是内置推荐，若新版本未收录可直接输入 `/model <模型名>`。");
    Ok(())
}

fn suggested_models(provider: &connect::ConnectProvider) -> Vec<&'static str> {
    match provider {
        connect::ConnectProvider::OpenAi => vec!["gpt-5", "gpt-4.1", "gpt-4.1-mini"],
        connect::ConnectProvider::Anthropic => {
            vec!["claude-3-7-sonnet-latest", "claude-3-5-sonnet-latest"]
        }
        connect::ConnectProvider::Zhipu => vec!["glm-4-plus", "glm-4-air", "glm-4-flash"],
    }
}

fn print_connect_status(paths: &AgentPaths) -> Result<()> {
    let cfg = connect::load(paths)?;
    let client = OpenAIClient::from_paths(paths, None)?;
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

fn print_skills_for_chat(paths: &AgentPaths) -> Result<()> {
    let list = skills::list_skills(paths)?;
    if list.is_empty() {
        println!("当前没有安装技能。");
    } else {
        let names = list.into_iter().map(|item| item.name).collect::<Vec<_>>();
        println!("可用技能：{}", names.join(", "));
    }
    Ok(())
}

fn suggest_skills(paths: &AgentPaths, prefix: &str) -> Result<Vec<String>> {
    let list = skills::list_skills(paths)?;
    let mut names = list
        .into_iter()
        .map(|item| item.name)
        .filter(|name| name.starts_with(prefix))
        .collect::<Vec<_>>();
    names.sort();
    Ok(names)
}

fn command_suggestions(input: &str) -> Vec<String> {
    let mut out = base_command_items()
        .into_iter()
        .map(|(label, _, _)| label.to_string())
        .filter(|cmd| cmd.starts_with(input))
        .collect::<Vec<_>>();
    out.sort();
    out
}

#[derive(Clone)]
struct HintItem {
    label: String,
    desc: String,
    completion: String,
}

fn base_command_items() -> Vec<(&'static str, &'static str, &'static str)> {
    vec![
        ("/help", "查看帮助", "/help"),
        ("/model", "查看/切换模型", "/model "),
        ("/connect", "连接模型后端", "/connect "),
        ("/skill", "使用技能", "/skill "),
        ("/clear", "清空当前屏幕", "/clear"),
        ("/exit", "退出对话", "/exit"),
    ]
}

fn single_command_hint(label: &str, desc: &str, completion: &str) -> Vec<HintItem> {
    vec![HintItem {
        label: label.to_string(),
        desc: desc.to_string(),
        completion: completion.to_string(),
    }]
}

fn connect_hint_items(rest: &str) -> Vec<HintItem> {
    let trimmed = rest.trim();
    let top_level = [
        ("openai", "OpenAI（login/api）", "/connect openai "),
        (
            "anthropic",
            "Anthropic（Claude，api）",
            "/connect anthropic ",
        ),
        ("zhipu", "智谱 GLM（api）", "/connect zhipu "),
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
                    _ => format!("/connect {} ", provider_command_name(&provider)),
                };
                let desc = match *method {
                    "login" => "使用登录态（仅 OpenAI）",
                    "api" => "使用 API Key",
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

    if tokens.len() == 2 && !methods.iter().any(|m| *m == method_token) {
        let mut items = methods
            .iter()
            .filter(|method| method.starts_with(method_token))
            .map(|method| {
                let completion = match *method {
                    "login" => format!("/connect {provider_cmd} login"),
                    "api" => format!("/connect {provider_cmd} api "),
                    _ => format!("/connect {provider_cmd} "),
                };
                let desc = match *method {
                    "login" => "使用登录态（仅 OpenAI）",
                    "api" => "使用 API Key",
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
            if !matches!(provider, connect::ConnectProvider::OpenAi) {
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
                for model in suggested_models(&connect::ConnectProvider::OpenAi) {
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
            for model in suggested_models(&connect::ConnectProvider::OpenAi) {
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

fn model_hint_items(paths: &AgentPaths, rest: &str) -> Vec<HintItem> {
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

fn command_inline_hint_items(paths: &AgentPaths, input: &str) -> Vec<HintItem> {
    if !input.starts_with('/') {
        return Vec::new();
    }

    if input == "/" {
        return base_command_items()
            .into_iter()
            .map(|(label, desc, completion)| HintItem {
                label: label.to_string(),
                desc: desc.to_string(),
                completion: completion.to_string(),
            })
            .collect();
    }

    if input == "/help" {
        return single_command_hint("/help", "按 Enter 执行查看帮助", "/help");
    }

    if input == "/clear" {
        return single_command_hint("/clear", "按 Enter 清空屏幕", "/clear");
    }

    if input == "/exit" {
        return single_command_hint("/exit", "按 Enter 退出对话", "/exit");
    }

    if input == "/quit" {
        return single_command_hint("/quit", "按 Enter 退出对话", "/quit");
    }

    if input == "/connect" {
        return single_command_hint("/connect", "按 Enter 或 Tab 进入连接设置", "/connect ");
    }

    if let Some(rest) = input.strip_prefix("/connect ") {
        return connect_hint_items(rest);
    }

    if input == "/model" {
        return single_command_hint("/model", "按 Enter 或 Tab 查看可选模型", "/model ");
    }

    if let Some(rest) = input.strip_prefix("/model ") {
        return model_hint_items(paths, rest);
    }

    if input == "/skill" {
        return single_command_hint("/skill", "按 Enter 或 Tab 进入 skill 选择", "/skill ");
    }

    if let Some(prefix) = input.strip_prefix("/skill ") {
        if prefix.contains(' ') {
            return Vec::new();
        }
        let skills = match skills::list_skills(paths) {
            Ok(v) => v,
            Err(_) => return Vec::new(),
        };
        let mut items = skills
            .into_iter()
            .filter(|item| item.name.starts_with(prefix))
            .take(10)
            .map(|item| HintItem {
                label: item.name.clone(),
                desc: item.description,
                completion: format!("/skill {} ", item.name),
            })
            .collect::<Vec<_>>();

        if items.is_empty() {
            items.push(HintItem {
                label: "未匹配到 skill".to_string(),
                desc: "可输入 /skill 查看可用技能".to_string(),
                completion: input.to_string(),
            });
        }
        return items;
    }

    let mut items = base_command_items()
        .into_iter()
        .filter(|(label, _, _)| label.starts_with(input))
        .map(|(label, desc, completion)| HintItem {
            label: label.to_string(),
            desc: desc.to_string(),
            completion: completion.to_string(),
        })
        .collect::<Vec<_>>();

    if items.is_empty() && "/skill ".starts_with(input) {
        items.push(HintItem {
            label: "/skill".to_string(),
            desc: "使用技能".to_string(),
            completion: "/skill ".to_string(),
        });
    }

    if items.is_empty() && "/connect ".starts_with(input) {
        items.push(HintItem {
            label: "/connect".to_string(),
            desc: "连接模型后端".to_string(),
            completion: "/connect ".to_string(),
        });
    }

    if items.is_empty() && "/model ".starts_with(input) {
        items.push(HintItem {
            label: "/model".to_string(),
            desc: "查看/切换模型".to_string(),
            completion: "/model ".to_string(),
        });
    }

    items
}

fn readline_with_inline_hint(paths: &AgentPaths, prompt: &str) -> io::Result<Option<String>> {
    if unsafe { libc::isatty(libc::STDIN_FILENO) } != 1 {
        let mut stdout = io::stdout();
        write!(stdout, "{prompt}")?;
        stdout.flush()?;
        let mut line = String::new();
        let read = io::stdin().read_line(&mut line)?;
        if read == 0 {
            return Ok(None);
        }
        let trimmed = line.trim_end_matches(['\r', '\n']).to_string();
        return Ok(Some(trimmed));
    }

    let _raw = RawMode::new()?;
    let mut stdout = io::stdout();
    let mut stdin = io::stdin();

    let mut input = String::new();
    let mut pending_utf8 = Vec::<u8>::new();
    let mut shown_hint_lines = 0usize;
    let mut selected = None;
    let mut hints = command_inline_hint_items(paths, &input);
    normalize_selected_index(&mut selected, hints.len());
    redraw_prompt_line(&mut stdout, prompt, &input)?;
    render_hint_panel(&mut stdout, &hints, selected, &mut shown_hint_lines)?;
    stdout.flush()?;

    loop {
        let mut byte = [0u8; 1];
        if stdin.read_exact(&mut byte).is_err() {
            render_hint_panel(&mut stdout, &[], None, &mut shown_hint_lines)?;
            write!(stdout, "\n")?;
            stdout.flush()?;
            return Ok(None);
        }

        match byte[0] {
            b'\r' | b'\n' => {
                if apply_selected_completion(&mut input, &hints, selected) {
                    hints = command_inline_hint_items(paths, &input);
                    normalize_selected_index(&mut selected, hints.len());
                    redraw_prompt_line(&mut stdout, prompt, &input)?;
                    render_hint_panel(&mut stdout, &hints, selected, &mut shown_hint_lines)?;
                    stdout.flush()?;
                    continue;
                }
                render_hint_panel(&mut stdout, &[], None, &mut shown_hint_lines)?;
                write!(stdout, "\n")?;
                stdout.flush()?;
                return Ok(Some(input));
            }
            b'\t' => {
                if apply_selected_completion(&mut input, &hints, selected) {
                    hints = command_inline_hint_items(paths, &input);
                    normalize_selected_index(&mut selected, hints.len());
                    redraw_prompt_line(&mut stdout, prompt, &input)?;
                    render_hint_panel(&mut stdout, &hints, selected, &mut shown_hint_lines)?;
                    stdout.flush()?;
                }
                continue;
            }
            27 => {
                let mut seq = [0u8; 2];
                if stdin.read_exact(&mut seq).is_ok() && seq[0] == b'[' {
                    match seq[1] {
                        b'A' => move_selection_up(&mut selected, hints.len()),
                        b'B' => move_selection_down(&mut selected, hints.len()),
                        b'C' => {
                            if apply_selected_completion(&mut input, &hints, selected) {
                                hints = command_inline_hint_items(paths, &input);
                                normalize_selected_index(&mut selected, hints.len());
                            }
                        }
                        _ => {}
                    }
                }
            }
            3 => {
                render_hint_panel(&mut stdout, &[], None, &mut shown_hint_lines)?;
                write!(stdout, "\n")?;
                stdout.flush()?;
                return Ok(None);
            }
            4 => {
                if input.is_empty() {
                    render_hint_panel(&mut stdout, &[], None, &mut shown_hint_lines)?;
                    write!(stdout, "\n")?;
                    stdout.flush()?;
                    return Ok(None);
                }
            }
            8 | 127 => {
                pending_utf8.clear();
                let _ = input.pop();
            }
            b if b < 32 => {}
            b => {
                pending_utf8.push(b);
                if let Ok(piece) = std::str::from_utf8(&pending_utf8) {
                    input.push_str(piece);
                    pending_utf8.clear();
                } else if pending_utf8.len() > 4 {
                    pending_utf8.clear();
                }
            }
        }

        hints = command_inline_hint_items(paths, &input);
        normalize_selected_index(&mut selected, hints.len());
        redraw_prompt_line(&mut stdout, prompt, &input)?;
        render_hint_panel(&mut stdout, &hints, selected, &mut shown_hint_lines)?;
        stdout.flush()?;
    }
}

fn render_hint_panel(
    stdout: &mut io::Stdout,
    hints: &[HintItem],
    selected: Option<usize>,
    shown_hint_lines: &mut usize,
) -> io::Result<()> {
    let lines_to_touch = cmp::max(*shown_hint_lines, hints.len());
    write!(stdout, "\x1b[s")?;
    for idx in 0..lines_to_touch {
        write!(stdout, "\n\r\x1b[2K")?;
        if idx < hints.len() {
            let marker = if Some(idx) == selected { ">" } else { " " };
            write!(
                stdout,
                "{} {:<24} {}",
                marker, hints[idx].label, hints[idx].desc
            )?;
        }
    }
    write!(stdout, "\x1b[u")?;
    *shown_hint_lines = hints.len();
    Ok(())
}

fn redraw_prompt_line(stdout: &mut io::Stdout, prompt: &str, input: &str) -> io::Result<()> {
    write!(stdout, "\r\x1b[2K{prompt}{input}")?;
    Ok(())
}

fn normalize_selected_index(selected: &mut Option<usize>, len: usize) {
    if len == 0 {
        *selected = None;
        return;
    }
    *selected = Some((*selected).unwrap_or(0).min(len - 1));
}

fn move_selection_up(selected: &mut Option<usize>, len: usize) {
    if len == 0 {
        *selected = None;
        return;
    }
    *selected = Some(match *selected {
        Some(0) | None => len - 1,
        Some(i) => i - 1,
    });
}

fn move_selection_down(selected: &mut Option<usize>, len: usize) {
    if len == 0 {
        *selected = None;
        return;
    }
    *selected = Some(match *selected {
        Some(i) if i + 1 < len => i + 1,
        _ => 0,
    });
}

fn apply_selected_completion(
    input: &mut String,
    hints: &[HintItem],
    selected: Option<usize>,
) -> bool {
    let Some(idx) = selected else {
        return false;
    };
    let Some(item) = hints.get(idx) else {
        return false;
    };
    let target = item.completion.as_str();
    if target.is_empty() {
        return false;
    }
    if input == target || input == target.trim_end() {
        return false;
    }
    *input = target.to_string();
    true
}

fn prompt_line(prompt: &str) -> io::Result<String> {
    let mut stdout = io::stdout();
    write!(stdout, "{prompt}")?;
    stdout.flush()?;
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    Ok(line.trim_end_matches(['\r', '\n']).to_string())
}

struct RawMode {
    original: libc::termios,
}

impl RawMode {
    fn new() -> io::Result<Self> {
        let fd = libc::STDIN_FILENO;
        let mut original = unsafe { std::mem::zeroed::<libc::termios>() };
        if unsafe { libc::tcgetattr(fd, &mut original) } != 0 {
            return Err(io::Error::last_os_error());
        }

        let mut raw = original;
        raw.c_lflag &= !(libc::ICANON | libc::ECHO);
        raw.c_cc[libc::VMIN] = 1;
        raw.c_cc[libc::VTIME] = 0;

        if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw) } != 0 {
            return Err(io::Error::last_os_error());
        }

        Ok(Self { original })
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        let _ = unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &self.original) };
    }
}

fn silently_capture_before_compaction(paths: &AgentPaths, messages: &[ChatMessage]) -> Result<()> {
    if messages.len() < 14 {
        return Ok(());
    }

    let recent_user_texts = messages
        .iter()
        .rev()
        .filter(|m| m.role == "user")
        .take(6)
        .map(|m| m.content.clone())
        .collect::<Vec<_>>();

    for user_text in recent_user_texts {
        let _ = memory::auto_capture_long_term(paths, "chat.compaction", &user_text)?;
    }
    Ok(())
}

fn trim_history(messages: &mut Vec<ChatMessage>, max_non_system: usize) {
    if messages.is_empty() {
        return;
    }
    let system = messages[0].clone();
    let non_system = messages[1..].to_vec();
    let trimmed = if non_system.len() > max_non_system {
        non_system[non_system.len() - max_non_system..].to_vec()
    } else {
        non_system
    };

    messages.clear();
    messages.push(system);
    messages.extend(trimmed);
}

fn handle_cron_command(paths: &AgentPaths, command: CronCommand) -> Result<()> {
    match command {
        CronCommand::Add {
            schedule,
            command,
            name,
            retry_max,
        } => {
            let job = jobs::add_job(paths, schedule, command, name, retry_max)?;
            println!("Added job:");
            println!("id: {}", job.id);
            println!("name: {}", job.name);
            println!("schedule: {}", job.schedule);
            println!("command: {}", job.command);
            let event = format!(
                "用户创建了定时任务：name={}，schedule={}，command={}",
                job.name, job.schedule, job.command
            );
            memory::append_short_term(paths, "cron.add", &event)?;
            let _ = memory::auto_capture_event(paths, "cron.add", &event)?;
        }
        CronCommand::List => {
            let jobs = jobs::load_jobs(paths)?;
            if jobs.is_empty() {
                println!("当前没有定时任务。");
            } else {
                for job in jobs {
                    println!(
                        "{} | {} | {} | retry={} | {}",
                        job.id, job.name, job.schedule, job.retry_max, job.command
                    );
                }
            }
        }
        CronCommand::Remove { id } => {
            let removed = jobs::remove_job(paths, &id)?;
            if removed {
                println!("Removed job: {id}");
            } else {
                println!("Job not found: {id}");
            }
        }
    }
    Ok(())
}

async fn handle_skill_command(paths: &AgentPaths, command: SkillCommand) -> Result<()> {
    match command {
        SkillCommand::List => {
            let list = skills::list_skills(paths)?;
            if list.is_empty() {
                println!("当前没有安装技能。");
            } else {
                for item in list {
                    println!(
                        "{} | {} | {}",
                        item.name,
                        item.description,
                        item.path.display()
                    );
                }
            }
        }
        SkillCommand::New { name } => {
            let path = skills::create_skill(paths, &name)?;
            println!("已创建技能模板：{}", path.display());
            let event = format!("用户创建了技能：name={}，path={}", name, path.display());
            memory::append_short_term(paths, "skill.new", &event)?;
            let _ = memory::auto_capture_event(paths, "skill.new", &event)?;
        }
        SkillCommand::Run { name, input, model } => {
            let client = OpenAIClient::from_paths(paths, model)?;
            let response = run_skill_and_record(paths, &client, &name, &input).await?;
            println!("{response}");
        }
    }
    Ok(())
}

fn handle_connect_command(paths: &AgentPaths, command: ConnectCommand) -> Result<()> {
    match command {
        ConnectCommand::Status => {
            print_connect_status(paths)?;
        }
        ConnectCommand::Login { model } => {
            connect::set_login(paths, model)?;
            let client = OpenAIClient::from_paths(paths, None)?;
            println!("已切换连接方式：{}", client.backend_label());
        }
        ConnectCommand::Api {
            api_key,
            provider,
            model,
        } => {
            let provider = parse_provider_name(&provider)?;
            connect::set_provider_api(paths, provider, api_key, model)?;
            let client = OpenAIClient::from_paths(paths, None)?;
            println!("已切换连接方式：{}", client.backend_label());
        }
    }
    Ok(())
}

fn parse_provider_name(name: &str) -> Result<connect::ConnectProvider> {
    match name.trim().to_ascii_lowercase().as_str() {
        "openai" => Ok(connect::ConnectProvider::OpenAi),
        "zhipu" | "glm" => Ok(connect::ConnectProvider::Zhipu),
        "anthropic" | "claude" => Ok(connect::ConnectProvider::Anthropic),
        other => bail!("不支持的 provider: {other}。可选: openai, zhipu, anthropic"),
    }
}

async fn run_skill_and_record(
    paths: &AgentPaths,
    client: &OpenAIClient,
    name: &str,
    input: &str,
) -> Result<String> {
    let response = skills::run_skill(paths, client, name, input).await?;
    memory::append_short_term(
        paths,
        &format!("skill.{name}"),
        &format!("input:\n{input}\n\nresponse:\n{response}"),
    )?;
    memory::auto_capture_long_term(paths, &format!("skill.{name}"), input)?;
    Ok(response)
}

mod chat_actions;
mod cli;
mod config;
mod connect;
mod daemon;
mod hooks;
mod jobs;
mod memory;
mod notify;
mod provider;
mod scheduler;
mod shell;
mod skills;
mod usage;

use anyhow::{Result, bail};
use chat_actions::{execute_local_action, extract_local_action_from_response};
use clap::Parser;
use cli::{Cli, Commands, CronCommand, HookCommand, SkillCommand};
use config::AgentPaths;
use provider::{ChatMessage, ProviderClient};
use std::cmp;
use std::io::{self, Read, Write};

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let paths = AgentPaths::new()?;
    paths.ensure()?;
    memory::ensure_capability_declarations(&paths)?;

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
        Commands::Remind { message } => {
            run_remind_command(&paths, &message)?;
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
        Commands::Connect { command } => provider::handle_connect_command(&paths, command)?,
        Commands::Cron { command } => handle_cron_command(&paths, command)?,
        Commands::Hook { command } => handle_hook_command(&paths, command)?,
        Commands::Skill { command } => handle_skill_command(&paths, command).await?,
    }

    Ok(())
}

async fn run_task(paths: &AgentPaths, task: &str, model: Option<String>) -> Result<()> {
    let client = ProviderClient::from_paths(paths, model)?;
    let _ = memory::capture_explicit_remember(paths, "run.task", task)?;
    let system = build_system_prompt(paths, &client, true)?;

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

fn run_remind_command(paths: &AgentPaths, message: &str) -> Result<()> {
    let msg = message.trim();
    if msg.is_empty() {
        println!("提醒内容为空，已忽略。");
        return Ok(());
    }

    println!("{msg}");
    let _ = notify::send_notification("GoldAgent 提醒", msg);
    memory::append_short_term(
        paths,
        "remind.fire",
        &format!(
            "time={}\nmessage={}",
            chrono::Local::now().to_rfc3339(),
            msg
        ),
    )?;
    Ok(())
}

async fn chat_loop(paths: &AgentPaths, model: Option<String>) -> Result<()> {
    let mut client = ProviderClient::from_paths(paths, model)?;
    let mut messages = vec![ChatMessage::system(build_system_prompt(
        paths, &client, false,
    )?)];

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
        let raw_response = client.chat(&messages).await?;
        let (action, cleaned_response, parse_error) =
            extract_local_action_from_response(&raw_response);
        let mut response = cleaned_response;

        if let Some(err) = parse_error {
            let msg = format!("本地动作解析失败：{err}");
            response = if response.trim().is_empty() {
                msg
            } else {
                format!("{msg}\n\n{response}")
            };
        }

        if let Some(action) = action {
            match execute_local_action(paths, action) {
                Ok(action_msg) => {
                    response = if response.trim().is_empty() {
                        action_msg
                    } else {
                        format!("{action_msg}\n\n{response}")
                    };
                }
                Err(err) => {
                    let msg = format!("本地动作执行失败：{err}");
                    response = if response.trim().is_empty() {
                        msg
                    } else {
                        format!("{msg}\n\n{response}")
                    };
                }
            }
        }

        if response.trim().is_empty() {
            response = "已执行。".to_string();
        }

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

fn print_chat_header(client: &ProviderClient) {
    println!();
    println!("  ____  ___  _     ____    _    ____ _____ _   _ _____ ");
    println!(" / ___|/ _ \\| |   |  _ \\  / \\  / ___| ____| \\ | |_   _|");
    println!("| |  _| | | | |   | | | |/ _ \\| |  _|  _| |  \\| | | |  ");
    println!("| |_| | |_| | |___| |_| / ___ \\ |_| | |___| |\\  | | |  ");
    println!(" \\____|\\___/|_____|____/_/   \\_\\____|_____|_| \\_| |_|  ");
    println!();
    println!("[GoldAgent] Chat session started");
    println!("[Backend] {}", client.backend_label());
}

fn print_chat_commands_hint() {
    println!("输入 `/` 可查看命令。");
    println!();
}

fn print_assistant_block(response: &str) {
    let mut lines = response.lines();
    match lines.next() {
        Some(first) => {
            println!("goldagent: {first}");
            for line in lines {
                println!("           {line}");
            }
        }
        None => {
            println!("goldagent:");
        }
    }
}

fn build_system_prompt(
    paths: &AgentPaths,
    client: &ProviderClient,
    concise: bool,
) -> Result<String> {
    let memory_context = memory::tail_context(paths, 4_000)?;
    let mut prompt = String::from("You are GoldAgent, a local assistant.\n");
    if concise {
        prompt.push_str("Use memory carefully and answer concisely.\n");
    } else {
        prompt.push_str(
            "Auto-execution protocol:\n\
When user asks to perform local operations (cron/hook), emit exactly one control line at the start of your reply:\n\
[[LOCAL_ACTION:{\"kind\":\"cron_add\",\"schedule\":\"daily@13:00\",\"task\":\"提醒我吃饭\"}]]\n\
Supported kinds:\n\
- cron_add {schedule, task, optional name, optional retry_max}\n\
- cron_list {}\n\
- cron_remove {id}\n\
- hook_add_git {repo, task, optional reference, optional interval_secs, optional name, optional retry_max, optional rules_file(LLM审查模式：填规则文件路径，设置后忽略task), optional report_file}\n\
- hook_add_p4 {depot, task, optional interval_secs, optional name, optional retry_max, optional rules_file(LLM审查模式：填规则文件路径，设置后忽略task), optional report_file}\n\
- hook_list {}\n\
- hook_remove {id}\n\
- hook_rules_new {optional path}\n\
Rules:\n\
- If required fields are missing or ambiguous, ask follow-up questions and DO NOT emit LOCAL_ACTION.\n\
- If the user clearly requests execution, prefer emitting LOCAL_ACTION rather than giving command suggestions.\n\n",
        );
    }
    prompt.push_str(&format!(
        "Current backend: {}.\n\
If asked about model/backend identity, answer strictly based on Current backend, not historical memory.\n\
Never claim a fixed model family unless it matches Current backend.\n\n\
Memory context:\n{}",
        client.backend_label(),
        memory_context
    ));
    Ok(prompt)
}

fn refresh_chat_system_prompt(
    paths: &AgentPaths,
    client: &ProviderClient,
    messages: &mut Vec<ChatMessage>,
) -> Result<()> {
    let system = ChatMessage::system(build_system_prompt(paths, client, false)?);
    if messages.is_empty() {
        messages.push(system);
    } else {
        messages[0] = system;
    }
    Ok(())
}

enum SlashAction {
    Continue,
    Exit,
}

async fn handle_chat_slash(
    paths: &AgentPaths,
    client: &mut ProviderClient,
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
        provider::print_connect_help(paths)?;
        return Ok(SlashAction::Continue);
    }

    if let Some(rest) = input.strip_prefix("/connect ") {
        let outcome = provider::handle_connect_chat_command(paths, client, rest, prompt_line)?;
        if outcome.handled {
            if outcome.client_changed {
                refresh_chat_system_prompt(paths, client, messages)?;
            }
            return Ok(SlashAction::Continue);
        }
    }

    let model_outcome = provider::handle_model_chat_command(paths, client, input)?;
    if model_outcome.handled {
        if model_outcome.client_changed {
            refresh_chat_system_prompt(paths, client, messages)?;
        }
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
    provider::print_connect_status(paths)?;
    print_skills_for_chat(paths)?;
    println!();
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

type HintItem = provider::HintItem;

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
        return provider::connect_hint_items(rest);
    }

    if input == "/model" {
        return single_command_hint("/model", "按 Enter 或 Tab 查看可选模型", "/model ");
    }

    if let Some(rest) = input.strip_prefix("/model ") {
        return provider::model_hint_items(paths, rest);
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
    if !stdin_is_tty() {
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

#[cfg(unix)]
fn stdin_is_tty() -> bool {
    unsafe { libc::isatty(libc::STDIN_FILENO) == 1 }
}

#[cfg(not(unix))]
fn stdin_is_tty() -> bool {
    false
}

#[cfg(unix)]
struct RawMode {
    original: libc::termios,
}

#[cfg(unix)]
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

#[cfg(unix)]
impl Drop for RawMode {
    fn drop(&mut self) {
        let _ = unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &self.original) };
    }
}

#[cfg(not(unix))]
struct RawMode;

#[cfg(not(unix))]
impl RawMode {
    fn new() -> io::Result<Self> {
        Ok(Self)
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

fn print_scheduler_auto_start_result(paths: &AgentPaths) {
    match daemon::ensure_scheduler_running(paths) {
        Ok(daemon::SchedulerStatus::Started(pid)) => {
            println!("已自动启动调度服务（pid={pid}）。");
        }
        Ok(daemon::SchedulerStatus::Reloaded(pid)) => {
            println!("已重载调度服务以应用新任务（pid={pid}）。");
        }
        Err(err) => {
            eprintln!("警告：任务已创建，但自动启动调度服务失败：{err}");
            eprintln!("请手动执行：goldagent serve");
        }
    }
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
            print_scheduler_auto_start_result(paths);
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

fn handle_hook_command(paths: &AgentPaths, command: HookCommand) -> Result<()> {
    match command {
        HookCommand::AddGit {
            repo,
            command,
            rules_file,
            report_file,
            reference,
            interval,
            name,
            retry_max,
        } => {
            if command.is_none() && rules_file.is_none() {
                bail!("必须提供 --command 或 --rules-file 之一");
            }
            let command = command.unwrap_or_default();
            let hook = hooks::add_git_hook(
                paths,
                repo,
                reference,
                interval,
                command,
                name,
                retry_max,
                rules_file,
                report_file,
            )?;
            println!("Added hook:");
            println!("id: {}", hook.id);
            println!("name: {}", hook.name);
            println!("source: {}", hook.source.as_str());
            println!("target: {}", hook.target);
            println!("reference: {}", hook.reference.as_deref().unwrap_or("HEAD"));
            println!("interval_secs: {}", hook.interval_secs);
            if let Some(ref rf) = hook.rules_file {
                println!("rules_file: {rf}");
                println!(
                    "report_file: {}",
                    hook.report_file
                        .as_deref()
                        .unwrap_or("<target>/goldagent-review.md")
                );
            } else {
                println!("command: {}", hook.command);
            }
            print_scheduler_auto_start_result(paths);
            let event = format!(
                "用户创建了 hook：name={}，source={}，target={}，rules_file={:?}，command={}",
                hook.name,
                hook.source.as_str(),
                hook.target,
                hook.rules_file,
                hook.command
            );
            memory::append_short_term(paths, "hook.add", &event)?;
            let _ = memory::auto_capture_event(paths, "hook.add", &event)?;
        }
        HookCommand::AddP4 {
            depot,
            command,
            rules_file,
            report_file,
            interval,
            name,
            retry_max,
        } => {
            if command.is_none() && rules_file.is_none() {
                bail!("必须提供 --command 或 --rules-file 之一");
            }
            let command = command.unwrap_or_default();
            let hook = hooks::add_p4_hook(
                paths,
                depot,
                interval,
                command,
                name,
                retry_max,
                rules_file,
                report_file,
            )?;
            println!("Added hook:");
            println!("id: {}", hook.id);
            println!("name: {}", hook.name);
            println!("source: {}", hook.source.as_str());
            println!("target: {}", hook.target);
            println!("interval_secs: {}", hook.interval_secs);
            if let Some(ref rf) = hook.rules_file {
                println!("rules_file: {rf}");
                println!(
                    "report_file: {}",
                    hook.report_file
                        .as_deref()
                        .unwrap_or("<target>/goldagent-review.md")
                );
            } else {
                println!("command: {}", hook.command);
            }
            print_scheduler_auto_start_result(paths);
            let event = format!(
                "用户创建了 hook：name={}，source={}，target={}，rules_file={:?}，command={}",
                hook.name,
                hook.source.as_str(),
                hook.target,
                hook.rules_file,
                hook.command
            );
            memory::append_short_term(paths, "hook.add", &event)?;
            let _ = memory::auto_capture_event(paths, "hook.add", &event)?;
        }
        HookCommand::List => {
            let hooks = hooks::load_hooks(paths)?;
            if hooks.is_empty() {
                println!("当前没有 hook 任务。");
            } else {
                for hook in hooks {
                    println!(
                        "{} | {} | {} | target={} | ref={} | interval={}s | retry={} | {}",
                        hook.id,
                        hook.name,
                        hook.source.as_str(),
                        hook.target,
                        hook.reference.as_deref().unwrap_or("-"),
                        hook.interval_secs,
                        hook.retry_max,
                        hook.command
                    );
                }
            }
        }
        HookCommand::Remove { id } => {
            let removed = hooks::remove_hook(paths, &id)?;
            if removed {
                println!("Removed hook: {id}");
            } else {
                println!("Hook not found: {id}");
            }
        }
        HookCommand::RulesNew { path } => {
            hooks::write_rules_template(&path)?;
            println!("已生成规则模板：{path}");
            println!("编辑完成后，用以下命令创建 hook：");
            println!("  goldagent hook add-git <repo> --ref main --rules-file {path}");
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
            let client = ProviderClient::from_paths(paths, model)?;
            let response = run_skill_and_record(paths, &client, &name, &input).await?;
            println!("{response}");
        }
    }
    Ok(())
}

async fn run_skill_and_record(
    paths: &AgentPaths,
    client: &ProviderClient,
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

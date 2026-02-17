mod cli;
mod config;
mod jobs;
mod memory;
mod openai;
mod scheduler;
mod shell;
mod skills;

use anyhow::Result;
use clap::Parser;
use cli::{Cli, Commands, CronCommand, SkillCommand};
use config::AgentPaths;
use openai::{ChatMessage, OpenAIClient};
use std::io::{self, Write};

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let paths = AgentPaths::new()?;
    paths.ensure()?;

    let command = cli.command.unwrap_or(Commands::Chat { model: None });

    match command {
        Commands::Init => {
            println!("GoldAgent initialized at {}", paths.root.display());
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
        Commands::Cron { command } => handle_cron_command(&paths, command)?,
        Commands::Skill { command } => handle_skill_command(&paths, command).await?,
    }

    Ok(())
}

async fn run_task(paths: &AgentPaths, task: &str, model: Option<String>) -> Result<()> {
    let client = OpenAIClient::from_env(model)?;
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
    let client = OpenAIClient::from_env(model)?;
    let memory_context = memory::tail_context(paths, 4_000)?;

    println!("GoldAgent chat started.");
    println!("Commands:");
    println!("- /exit");
    println!("- /shell <command>");
    println!();

    let mut messages = vec![ChatMessage::system(format!(
        "You are GoldAgent, a local assistant.\nMemory context:\n{memory_context}"
    ))];

    loop {
        print!("you> ");
        io::stdout().flush()?;
        let mut line = String::new();
        io::stdin().read_line(&mut line)?;
        let input = line.trim();

        if input.is_empty() {
            continue;
        }

        if input == "/exit" || input == "/quit" {
            break;
        }

        if let Some(rest) = input.strip_prefix("/shell ") {
            match shell::run_shell_command(rest.trim(), false).await {
                Ok(out) => {
                    if !out.stdout.trim().is_empty() {
                        println!("{}", out.stdout.trim_end());
                    }
                    if !out.stderr.trim().is_empty() {
                        eprintln!("{}", out.stderr.trim_end());
                    }
                }
                Err(err) => eprintln!("{err}"),
            }
            continue;
        }

        let _ = memory::capture_explicit_remember(paths, "chat.turn", input)?;
        messages.push(ChatMessage::user(input));
        let response = client.chat(&messages).await?;
        println!("goldagent> {response}");
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

    Ok(())
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
                println!("No jobs found.");
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
            let skills = skills::list_skills(paths)?;
            if skills.is_empty() {
                println!("No skills installed.");
            } else {
                for skill in skills {
                    println!("{} | {} | {}", skill.name, skill.description, skill.path.display());
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
            let client = OpenAIClient::from_env(model)?;
            let response = skills::run_skill(paths, &client, &name, &input).await?;
            println!("{response}");
            memory::append_short_term(
                paths,
                &format!("skill.{name}"),
                &format!("input:\n{input}\n\nresponse:\n{response}"),
            )?;
            memory::auto_capture_long_term(paths, &format!("skill.{name}"), &input)?;
        }
    }
    Ok(())
}

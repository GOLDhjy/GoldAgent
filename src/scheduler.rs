use crate::config::AgentPaths;
use crate::hooks::{self, Hook};
use crate::jobs::{self, Job};
use crate::memory;
use crate::shell;
use anyhow::Result;
use chrono::Local;
use cron::Schedule;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::str::FromStr;
use tokio::signal;
use tokio::time::{Duration, sleep};

pub async fn serve(paths: AgentPaths) -> Result<()> {
    let Some(_pid_guard) = SchedulerPidGuard::acquire(&paths)? else {
        if let Some(pid) = running_pid(&paths)? {
            println!("GoldAgent scheduler already running (pid={pid}).");
        } else {
            println!("GoldAgent scheduler already running.");
        }
        return Ok(());
    };

    let jobs = jobs::load_jobs(&paths)?;
    let hooks = hooks::load_hooks(&paths)?;

    if jobs.is_empty() && hooks.is_empty() {
        println!(
            "No cron jobs or hooks configured. Add one with `goldagent cron add ...` or `goldagent hook add-git ...`"
        );
    } else {
        println!(
            "Loaded {} cron job(s) and {} hook watcher(s).",
            jobs.iter().filter(|j| j.enabled).count(),
            hooks.iter().filter(|h| h.enabled).count()
        );
    }

    for job in jobs.into_iter().filter(|j| j.enabled) {
        let paths_clone = paths.clone();
        tokio::spawn(async move {
            if let Err(err) = run_job_loop(paths_clone, job).await {
                eprintln!("Scheduler task exited with error: {err}");
            }
        });
    }

    for hook in hooks.into_iter().filter(|h| h.enabled) {
        let paths_clone = paths.clone();
        tokio::spawn(async move {
            if let Err(err) = run_hook_loop(paths_clone, hook).await {
                eprintln!("Hook watcher exited with error: {err}");
            }
        });
    }

    println!("GoldAgent scheduler is running. Press Ctrl+C to stop.");
    signal::ctrl_c().await?;
    println!("GoldAgent scheduler stopped.");
    Ok(())
}

pub fn running_pid(paths: &AgentPaths) -> Result<Option<u32>> {
    let pid_file = scheduler_pid_file(paths);
    let raw = match fs::read_to_string(&pid_file) {
        Ok(value) => value,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err.into()),
    };

    let trimmed = raw.trim();
    let Ok(pid) = trimmed.parse::<u32>() else {
        let _ = fs::remove_file(&pid_file);
        return Ok(None);
    };

    if process_is_alive(pid) {
        Ok(Some(pid))
    } else {
        let _ = fs::remove_file(&pid_file);
        Ok(None)
    }
}

fn scheduler_pid_file(paths: &AgentPaths) -> PathBuf {
    paths.root.join("scheduler.pid")
}

struct SchedulerPidGuard {
    path: PathBuf,
}

impl SchedulerPidGuard {
    fn acquire(paths: &AgentPaths) -> Result<Option<Self>> {
        if let Some(pid) = running_pid(paths)?
            && pid != std::process::id()
        {
            return Ok(None);
        }

        let path = scheduler_pid_file(paths);
        fs::write(&path, format!("{}\n", std::process::id()))?;
        Ok(Some(Self { path }))
    }
}

impl Drop for SchedulerPidGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[cfg(unix)]
fn process_is_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    let rc = unsafe { libc::kill(pid as i32, 0) };
    if rc == 0 {
        true
    } else {
        matches!(
            io::Error::last_os_error().raw_os_error(),
            Some(code) if code == libc::EPERM
        )
    }
}

#[cfg(not(unix))]
fn process_is_alive(_pid: u32) -> bool {
    false
}

async fn run_job_loop(paths: AgentPaths, job: Job) -> Result<()> {
    let normalized = jobs::normalize_schedule(&job.schedule)?;
    let schedule = Schedule::from_str(&normalized)?;
    let mut upcoming = schedule.after(&Local::now());

    loop {
        let Some(next) = upcoming.next() else {
            break;
        };

        let now = Local::now();
        if next > now {
            let wait = (next - now)
                .to_std()
                .unwrap_or_else(|_| Duration::from_secs(0));
            sleep(wait).await;
        }

        execute_with_retry(&paths, &job).await;
    }

    Ok(())
}

async fn execute_with_retry(paths: &AgentPaths, job: &Job) {
    let effective_command = effective_job_command(&job.command);

    for attempt in 0..=job.retry_max {
        let result = shell::run_shell_command(&effective_command, false).await;

        match result {
            Ok(output) => {
                let log_line = format!(
                    "job={} name={} status=success code={}\nstdout:\n{}\nstderr:\n{}",
                    job.id, job.name, output.exit_code, output.stdout, output.stderr
                );
                let _ = memory::append_short_term(paths, &format!("cron.{}", job.id), &log_line);
                return;
            }
            Err(err) => {
                let is_last = attempt == job.retry_max;
                let log_line = format!(
                    "job={} name={} status=failed attempt={}/{}\nerror={}",
                    job.id,
                    job.name,
                    attempt + 1,
                    job.retry_max + 1,
                    err
                );
                let _ = memory::append_short_term(paths, &format!("cron.{}", job.id), &log_line);

                if is_last {
                    eprintln!("Job {} ({}) failed after retries: {err}", job.id, job.name);
                    return;
                }
                sleep(Duration::from_secs(3)).await;
            }
        }
    }
}

fn effective_job_command(command: &str) -> String {
    let Some(message) = parse_goldagent_run_message(command) else {
        return command.to_string();
    };

    if is_reminder_message(&message) {
        build_goldagent_remind_command(&message)
    } else {
        command.to_string()
    }
}

fn parse_goldagent_run_message(command: &str) -> Option<String> {
    let trimmed = command.trim();
    let content = trimmed
        .strip_prefix("goldagent run \"")?
        .strip_suffix('\"')?;
    Some(unescape_quoted(content))
}

fn unescape_quoted(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            if let Some(next) = chars.next() {
                out.push(next);
            } else {
                out.push(ch);
            }
        } else {
            out.push(ch);
        }
    }
    out
}

fn is_reminder_message(message: &str) -> bool {
    let trimmed = message.trim();
    trimmed.starts_with("提醒")
        || trimmed.starts_with("到点")
        || trimmed.to_ascii_lowercase().starts_with("remind")
}

fn build_goldagent_remind_command(message: &str) -> String {
    let normalized = message.replace(['\r', '\n'], " ");
    let escaped = normalized.replace('\\', "\\\\").replace('"', "\\\"");
    format!("goldagent remind \"{}\"", escaped.trim())
}

async fn run_hook_loop(paths: AgentPaths, hook: Hook) -> Result<()> {
    let mut last_seen = match hooks::read_signature(&hook).await {
        Ok(signature) => signature,
        Err(err) => {
            eprintln!(
                "Hook {} ({}) initial poll failed: {err}",
                hook.id, hook.name
            );
            String::new()
        }
    };

    loop {
        sleep(Duration::from_secs(hook.interval_secs)).await;
        match hooks::read_signature(&hook).await {
            Ok(current) => {
                if last_seen.is_empty() {
                    last_seen = current;
                    continue;
                }

                if current != last_seen {
                    execute_hook_with_retry(&paths, &hook, &last_seen, &current).await;
                    last_seen = current;
                }
            }
            Err(err) => {
                eprintln!("Hook {} ({}) poll failed: {err}", hook.id, hook.name);
            }
        }
    }
}

async fn execute_hook_with_retry(paths: &AgentPaths, hook: &Hook, previous: &str, current: &str) {
    let command = hooks::render_command_template(hook, previous, current);
    for attempt in 0..=hook.retry_max {
        let result = shell::run_shell_command(&command, false).await;

        match result {
            Ok(output) => {
                let log_line = format!(
                    "hook={} name={} source={} status=success\nprevious={}\ncurrent={}\ncommand={}\nstdout:\n{}\nstderr:\n{}",
                    hook.id,
                    hook.name,
                    hook.source.as_str(),
                    previous,
                    current,
                    command,
                    output.stdout,
                    output.stderr
                );
                let _ = memory::append_short_term(paths, &format!("hook.{}", hook.id), &log_line);
                return;
            }
            Err(err) => {
                let is_last = attempt == hook.retry_max;
                let log_line = format!(
                    "hook={} name={} source={} status=failed attempt={}/{}\nprevious={}\ncurrent={}\ncommand={}\nerror={}",
                    hook.id,
                    hook.name,
                    hook.source.as_str(),
                    attempt + 1,
                    hook.retry_max + 1,
                    previous,
                    current,
                    command,
                    err
                );
                let _ = memory::append_short_term(paths, &format!("hook.{}", hook.id), &log_line);

                if is_last {
                    eprintln!(
                        "Hook {} ({}) failed after retries: {err}",
                        hook.id, hook.name
                    );
                    return;
                }
                sleep(Duration::from_secs(3)).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::effective_job_command;

    #[test]
    fn upgrades_legacy_run_reminder_command() {
        let out = effective_job_command("goldagent run \"提醒我喝水\"");
        assert_eq!(out, "goldagent remind \"提醒我喝水\"");
    }

    #[test]
    fn keeps_non_reminder_run_command() {
        let out = effective_job_command("goldagent run \"总结今天工作\"");
        assert_eq!(out, "goldagent run \"总结今天工作\"");
    }
}

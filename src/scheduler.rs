use crate::config::AgentPaths;
use crate::jobs::{self, Job};
use crate::memory;
use crate::shell;
use anyhow::Result;
use chrono::Utc;
use cron::Schedule;
use std::str::FromStr;
use tokio::signal;
use tokio::time::{Duration, sleep};

pub async fn serve(paths: AgentPaths) -> Result<()> {
    let jobs = jobs::load_jobs(&paths)?;
    if jobs.is_empty() {
        println!("No cron jobs configured. Add one with `goldagent cron add ...`");
    }

    for job in jobs.into_iter().filter(|j| j.enabled) {
        let paths_clone = paths.clone();
        tokio::spawn(async move {
            if let Err(err) = run_job_loop(paths_clone, job).await {
                eprintln!("Scheduler task exited with error: {err}");
            }
        });
    }

    println!("GoldAgent scheduler is running. Press Ctrl+C to stop.");
    signal::ctrl_c().await?;
    println!("GoldAgent scheduler stopped.");
    Ok(())
}

async fn run_job_loop(paths: AgentPaths, job: Job) -> Result<()> {
    let normalized = jobs::normalize_schedule(&job.schedule)?;
    let schedule = Schedule::from_str(&normalized)?;
    let mut upcoming = schedule.after(&Utc::now());

    loop {
        let Some(next) = upcoming.next() else {
            break;
        };

        let now = Utc::now();
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
    for attempt in 0..=job.retry_max {
        let result = shell::run_shell_command(&job.command, false).await;

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

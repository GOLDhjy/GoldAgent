use crate::config::AgentPaths;
use anyhow::{Context, Result, bail};
use chrono::Utc;
use cron::Schedule;
use serde::{Deserialize, Serialize};
use std::fs;
use std::str::FromStr;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    pub name: String,
    pub schedule: String,
    pub command: String,
    pub enabled: bool,
    pub retry_max: u8,
    pub created_at: String,
}

pub fn load_jobs(paths: &AgentPaths) -> Result<Vec<Job>> {
    let raw = fs::read_to_string(&paths.jobs_file).unwrap_or_else(|_| "[]".to_string());
    let jobs = serde_json::from_str::<Vec<Job>>(&raw)
        .with_context(|| format!("Failed to parse jobs file {}", paths.jobs_file.display()))?;
    Ok(jobs)
}

pub fn add_job(
    paths: &AgentPaths,
    schedule: String,
    command: String,
    name: Option<String>,
    retry_max: u8,
) -> Result<Job> {
    validate_schedule(&schedule)?;

    let mut jobs = load_jobs(paths)?;
    let id = Uuid::new_v4().to_string();
    let job = Job {
        id: id.clone(),
        name: name.unwrap_or_else(|| format!("job-{id}")),
        schedule,
        command,
        enabled: true,
        retry_max,
        created_at: Utc::now().to_rfc3339(),
    };
    jobs.push(job.clone());
    save_jobs(paths, &jobs)?;
    Ok(job)
}

pub fn remove_job(paths: &AgentPaths, id: &str) -> Result<bool> {
    let mut jobs = load_jobs(paths)?;
    let before = jobs.len();
    jobs.retain(|job| job.id != id);
    let removed = jobs.len() != before;
    if removed {
        save_jobs(paths, &jobs)?;
    }
    Ok(removed)
}

fn save_jobs(paths: &AgentPaths, jobs: &[Job]) -> Result<()> {
    let serialized = serde_json::to_string_pretty(jobs)?;
    fs::write(&paths.jobs_file, serialized)?;
    Ok(())
}

pub fn normalize_schedule(expr: &str) -> Result<String> {
    let parts = expr.split_whitespace().collect::<Vec<_>>();
    match parts.len() {
        5 => Ok(format!("0 {expr}")),
        6 => Ok(expr.to_string()),
        _ => bail!(
            "Invalid cron expression `{expr}`. Expected 5 fields (min hour day month weekday) or 6 fields (sec min hour day month weekday)."
        ),
    }
}

pub fn validate_schedule(expr: &str) -> Result<()> {
    let normalized = normalize_schedule(expr)?;
    Schedule::from_str(&normalized).with_context(|| format!("Invalid cron expression: {expr}"))?;
    Ok(())
}

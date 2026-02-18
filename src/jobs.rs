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
    let expr = expr.trim();
    if let Some(time) = expr.strip_prefix("daily@") {
        let (hour, minute) = parse_hh_mm(time)?;
        return Ok(format!("0 {minute} {hour} * * *"));
    }
    if let Some(time) = expr.strip_prefix("weekdays@") {
        let (hour, minute) = parse_hh_mm(time)?;
        return Ok(format!("0 {minute} {hour} * * 1-5"));
    }

    let parts = expr.split_whitespace().collect::<Vec<_>>();
    match parts.len() {
        5 => Ok(format!("0 {expr}")),
        6 => Ok(expr.to_string()),
        _ => bail!(
            "Invalid schedule `{expr}`. Expected: 5-field cron (min hour day month weekday), 6-field cron (sec min hour day month weekday), `daily@HH:MM`, or `weekdays@HH:MM`."
        ),
    }
}

pub fn validate_schedule(expr: &str) -> Result<()> {
    let normalized = normalize_schedule(expr)?;
    Schedule::from_str(&normalized).with_context(|| format!("Invalid cron expression: {expr}"))?;
    Ok(())
}

fn parse_hh_mm(raw: &str) -> Result<(u8, u8)> {
    let Some((hour_raw, minute_raw)) = raw.split_once(':') else {
        bail!("Invalid time `{raw}`. Expected HH:MM.");
    };
    let hour = hour_raw
        .parse::<u8>()
        .with_context(|| format!("Invalid hour in `{raw}`"))?;
    let minute = minute_raw
        .parse::<u8>()
        .with_context(|| format!("Invalid minute in `{raw}`"))?;

    if hour > 23 {
        bail!("Invalid hour `{hour}`. Expected 00-23.");
    }
    if minute > 59 {
        bail!("Invalid minute `{minute}`. Expected 00-59.");
    }
    Ok((hour, minute))
}

#[cfg(test)]
mod tests {
    use super::normalize_schedule;

    #[test]
    fn normalizes_five_field_cron() {
        let out = normalize_schedule("0 13 * * *").expect("normalize should succeed");
        assert_eq!(out, "0 0 13 * * *");
    }

    #[test]
    fn keeps_six_field_cron() {
        let out = normalize_schedule("30 0 13 * * *").expect("normalize should succeed");
        assert_eq!(out, "30 0 13 * * *");
    }

    #[test]
    fn supports_daily_shortcut() {
        let out = normalize_schedule("daily@13:00").expect("normalize should succeed");
        assert_eq!(out, "0 0 13 * * *");
    }

    #[test]
    fn supports_weekdays_shortcut() {
        let out = normalize_schedule("weekdays@13:00").expect("normalize should succeed");
        assert_eq!(out, "0 0 13 * * 1-5");
    }

    #[test]
    fn rejects_invalid_shortcut_time() {
        let err = normalize_schedule("daily@25:00").expect_err("normalize should fail");
        assert!(err.to_string().contains("Invalid hour"));
    }
}

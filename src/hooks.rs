use crate::config::AgentPaths;
use crate::shell;
use anyhow::{Context, Result, bail};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::fs;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookSource {
    Git,
    P4,
}

impl HookSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Git => "git",
            Self::P4 => "p4",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hook {
    pub id: String,
    pub name: String,
    pub source: HookSource,
    pub target: String,
    pub reference: Option<String>,
    pub interval_secs: u64,
    pub command: String,
    pub enabled: bool,
    pub retry_max: u8,
    pub created_at: String,
}

pub fn load_hooks(paths: &AgentPaths) -> Result<Vec<Hook>> {
    let raw = fs::read_to_string(&paths.hooks_file).unwrap_or_else(|_| "[]".to_string());
    let hooks = serde_json::from_str::<Vec<Hook>>(&raw)
        .with_context(|| format!("Failed to parse hooks file {}", paths.hooks_file.display()))?;
    Ok(hooks)
}

pub fn add_git_hook(
    paths: &AgentPaths,
    repo: String,
    reference: Option<String>,
    interval_secs: u64,
    command: String,
    name: Option<String>,
    retry_max: u8,
) -> Result<Hook> {
    validate_interval(interval_secs)?;

    let mut hooks = load_hooks(paths)?;
    let id = Uuid::new_v4().to_string();
    let hook = Hook {
        id: id.clone(),
        name: name.unwrap_or_else(|| format!("hook-{id}")),
        source: HookSource::Git,
        target: repo,
        reference,
        interval_secs,
        command,
        enabled: true,
        retry_max,
        created_at: Utc::now().to_rfc3339(),
    };
    hooks.push(hook.clone());
    save_hooks(paths, &hooks)?;
    Ok(hook)
}

pub fn add_p4_hook(
    paths: &AgentPaths,
    depot: String,
    interval_secs: u64,
    command: String,
    name: Option<String>,
    retry_max: u8,
) -> Result<Hook> {
    validate_interval(interval_secs)?;

    let mut hooks = load_hooks(paths)?;
    let id = Uuid::new_v4().to_string();
    let hook = Hook {
        id: id.clone(),
        name: name.unwrap_or_else(|| format!("hook-{id}")),
        source: HookSource::P4,
        target: depot,
        reference: None,
        interval_secs,
        command,
        enabled: true,
        retry_max,
        created_at: Utc::now().to_rfc3339(),
    };
    hooks.push(hook.clone());
    save_hooks(paths, &hooks)?;
    Ok(hook)
}

pub fn remove_hook(paths: &AgentPaths, id: &str) -> Result<bool> {
    let mut hooks = load_hooks(paths)?;
    let before = hooks.len();
    hooks.retain(|hook| hook.id != id);
    let removed = hooks.len() != before;
    if removed {
        save_hooks(paths, &hooks)?;
    }
    Ok(removed)
}

pub async fn read_signature(hook: &Hook) -> Result<String> {
    match hook.source {
        HookSource::Git => read_git_signature(&hook.target, hook.reference.as_deref()).await,
        HookSource::P4 => read_p4_signature(&hook.target).await,
    }
}

pub fn render_command_template(hook: &Hook, previous: &str, current: &str) -> String {
    let reference = hook.reference.as_deref().unwrap_or("HEAD");
    hook.command
        .replace("${HOOK_ID}", &hook.id)
        .replace("${HOOK_NAME}", &hook.name)
        .replace("${HOOK_SOURCE}", hook.source.as_str())
        .replace("${HOOK_TARGET}", &hook.target)
        .replace("${HOOK_REF}", reference)
        .replace("${HOOK_PREVIOUS}", previous)
        .replace("${HOOK_CURRENT}", current)
}

fn save_hooks(paths: &AgentPaths, hooks: &[Hook]) -> Result<()> {
    let serialized = serde_json::to_string_pretty(hooks)?;
    fs::write(&paths.hooks_file, serialized)?;
    Ok(())
}

fn validate_interval(interval_secs: u64) -> Result<()> {
    if interval_secs == 0 {
        bail!("Invalid interval `0`. Expected >= 1 second.");
    }
    Ok(())
}

async fn read_git_signature(repo: &str, reference: Option<&str>) -> Result<String> {
    let reference = reference.unwrap_or("HEAD");
    let cmd = format!(
        "git -C {} rev-parse {}",
        shell_quote(repo),
        shell_quote(reference)
    );
    let output = shell::run_shell_command(&cmd, false).await?;
    let signature = output.stdout.trim();
    if signature.is_empty() {
        bail!("git rev-parse returned empty output for repo `{repo}`");
    }
    Ok(signature.to_string())
}

async fn read_p4_signature(depot: &str) -> Result<String> {
    let cmd = format!("p4 changes -m 1 {}", shell_quote(depot));
    let output = shell::run_shell_command(&cmd, false).await?;
    let Some(line) = output
        .stdout
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
    else {
        bail!("p4 changes returned empty output for depot `{depot}`");
    };
    Ok(line.to_string())
}

fn shell_quote(raw: &str) -> String {
    let escaped = raw.replace('\'', "'\"'\"'");
    format!("'{escaped}'")
}

#[cfg(test)]
mod tests {
    use super::{Hook, HookSource, render_command_template};

    #[test]
    fn renders_hook_placeholders() {
        let hook = Hook {
            id: "h1".to_string(),
            name: "my-hook".to_string(),
            source: HookSource::Git,
            target: "/tmp/repo".to_string(),
            reference: Some("main".to_string()),
            interval_secs: 30,
            command: "echo ${HOOK_SOURCE} ${HOOK_PREVIOUS} -> ${HOOK_CURRENT}".to_string(),
            enabled: true,
            retry_max: 1,
            created_at: "2025-01-01T00:00:00Z".to_string(),
        };
        let out = render_command_template(&hook, "a", "b");
        assert_eq!(out, "echo git a -> b");
    }
}

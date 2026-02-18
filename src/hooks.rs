use crate::config::AgentPaths;
use crate::shell;
use anyhow::{Context, Result, bail};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
use uuid::Uuid;

const RULES_TEMPLATE: &str = r#"# LLM 代码审查规则
# 本文件由 `goldagent hook rules-new` 生成，可随时编辑，无需重建 hook。

请审查以下代码变更，重点检查以下几个维度，并在末尾给出总体评价。

## 一、安全漏洞
- SQL 注入、命令注入、路径穿越
- XSS / CSRF
- 硬编码密钥、Token、密码
- 权限校验遗漏（未鉴权的接口、越权访问）
- 不安全的反序列化

## 二、代码质量
- 命名是否清晰（变量、函数、类）
- 函数是否过长（建议不超过 50 行）
- 重复代码（DRY 原则）
- 魔法数字 / 魔法字符串未抽取为常量

## 三、潜在 Bug
- 空指针 / 空引用未处理
- 数组越界风险
- 整数溢出
- 并发竞争条件（共享状态未加锁）
- 错误 / 异常未处理或被静默吞掉

## 四、性能
- N+1 查询
- 不必要的大对象复制
- 循环内的重复计算

## 输出格式要求
- 每个问题单独列出，格式：`文件名:行号 — 问题描述 — 修改建议`
- 按严重程度排序：严重 > 警告 > 建议
- 若无问题，直接回复：**未发现明显问题。**
- 结尾给出一句总体评价
"#;

pub fn write_rules_template(path: &str) -> Result<()> {
    let p = Path::new(path);
    if p.exists() {
        bail!("文件已存在：{path}，请删除后重试或指定其他路径。");
    }
    if let Some(parent) = p.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    fs::write(p, RULES_TEMPLATE)?;
    Ok(())
}

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
    /// Path to a Markdown file with review rules. When set the hook uses LLM
    /// code-review mode instead of executing `command`. The file is read on
    /// every trigger so the user can edit the rules without recreating the hook.
    #[serde(default)]
    pub rules_file: Option<String>,
    /// Where to append the LLM review report. Defaults to
    /// `<target>/goldagent-review.md` when absent.
    #[serde(default)]
    pub report_file: Option<String>,
}

pub fn load_hooks(paths: &AgentPaths) -> Result<Vec<Hook>> {
    let raw = fs::read_to_string(&paths.hooks_file).unwrap_or_else(|_| "[]".to_string());
    let hooks = serde_json::from_str::<Vec<Hook>>(&raw)
        .with_context(|| format!("Failed to parse hooks file {}", paths.hooks_file.display()))?;
    Ok(hooks)
}

#[allow(clippy::too_many_arguments)]
pub fn add_git_hook(
    paths: &AgentPaths,
    repo: String,
    reference: Option<String>,
    interval_secs: u64,
    command: String,
    name: Option<String>,
    retry_max: u8,
    rules_file: Option<String>,
    report_file: Option<String>,
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
        rules_file,
        report_file,
    };
    hooks.push(hook.clone());
    save_hooks(paths, &hooks)?;
    Ok(hook)
}

#[allow(clippy::too_many_arguments)]
pub fn add_p4_hook(
    paths: &AgentPaths,
    depot: String,
    interval_secs: u64,
    command: String,
    name: Option<String>,
    retry_max: u8,
    rules_file: Option<String>,
    report_file: Option<String>,
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
        rules_file,
        report_file,
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
            rules_file: None,
            report_file: None,
        };
        let out = render_command_template(&hook, "a", "b");
        assert_eq!(out, "echo git a -> b");
    }
}

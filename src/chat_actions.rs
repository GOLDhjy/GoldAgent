use crate::config::AgentPaths;
use crate::daemon;
use crate::hooks;
use crate::jobs;
use crate::memory;
use anyhow::Result;
use serde::Deserialize;

const LOCAL_ACTION_PREFIX: &str = "[[LOCAL_ACTION:";

#[derive(Debug, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum ChatLocalAction {
    CronAdd {
        schedule: String,
        task: String,
        #[serde(default)]
        name: Option<String>,
        #[serde(default = "default_retry_max")]
        retry_max: u8,
    },
    CronList,
    CronRemove {
        id: String,
    },
    HookAddGit {
        repo: String,
        task: String,
        #[serde(default)]
        reference: Option<String>,
        #[serde(default = "default_hook_interval_secs")]
        interval_secs: u64,
        #[serde(default)]
        name: Option<String>,
        #[serde(default = "default_retry_max")]
        retry_max: u8,
        #[serde(default)]
        rules_file: Option<String>,
        #[serde(default)]
        report_file: Option<String>,
    },
    HookAddP4 {
        depot: String,
        task: String,
        #[serde(default = "default_hook_interval_secs")]
        interval_secs: u64,
        #[serde(default)]
        name: Option<String>,
        #[serde(default = "default_retry_max")]
        retry_max: u8,
        #[serde(default)]
        rules_file: Option<String>,
        #[serde(default)]
        report_file: Option<String>,
    },
    HookList,
    HookRemove {
        id: String,
    },
    HookRulesNew {
        #[serde(default = "default_rules_path")]
        path: String,
    },
}

fn default_retry_max() -> u8 {
    1
}

fn default_hook_interval_secs() -> u64 {
    30
}

fn default_rules_path() -> String {
    "./review-rules.md".to_string()
}

pub(crate) fn build_run_task_command(task: &str) -> String {
    let normalized = task.replace(['\r', '\n'], " ");
    let escaped = normalized.replace('\\', "\\\\").replace('"', "\\\"");
    format!("goldagent run \"{}\"", escaped.trim())
}

fn build_remind_command(message: &str) -> String {
    let normalized = message.replace(['\r', '\n'], " ");
    let escaped = normalized.replace('\\', "\\\\").replace('"', "\\\"");
    format!("goldagent remind \"{}\"", escaped.trim())
}

fn is_reminder_task(task: &str) -> bool {
    let trimmed = task.trim();
    trimmed.starts_with("提醒")
        || trimmed.starts_with("到点")
        || trimmed.to_ascii_lowercase().starts_with("remind")
}

fn build_scheduled_task_command(task: &str) -> String {
    if is_reminder_task(task) {
        build_remind_command(task)
    } else {
        build_run_task_command(task)
    }
}

pub(crate) fn extract_local_action_from_response(
    raw: &str,
) -> (Option<ChatLocalAction>, String, Option<String>) {
    let mut action = None;
    let mut parse_error = None;
    let mut kept_lines = Vec::new();

    for line in raw.lines() {
        let trimmed = line.trim();
        if action.is_none() && trimmed.starts_with(LOCAL_ACTION_PREFIX) && trimmed.ends_with("]]") {
            let payload = &trimmed[LOCAL_ACTION_PREFIX.len()..trimmed.len() - 2];
            match serde_json::from_str::<ChatLocalAction>(payload) {
                Ok(parsed) => action = Some(parsed),
                Err(err) => parse_error = Some(err.to_string()),
            }
            continue;
        }
        kept_lines.push(line);
    }

    (
        action,
        kept_lines.join("\n").trim().to_string(),
        parse_error,
    )
}

pub(crate) fn execute_local_action(paths: &AgentPaths, action: ChatLocalAction) -> Result<String> {
    match action {
        ChatLocalAction::CronAdd {
            schedule,
            task,
            name,
            retry_max,
        } => {
            let command = build_scheduled_task_command(&task);
            let job = jobs::add_job(paths, schedule, command, name, retry_max)?;
            let event = format!(
                "用户通过聊天创建了定时任务：name={}，schedule={}，command={}",
                job.name, job.schedule, job.command
            );
            memory::append_short_term(paths, "cron.add", &event)?;
            let _ = memory::auto_capture_event(paths, "cron.add", &event)?;
            let scheduler_note = match daemon::ensure_scheduler_running(paths) {
                Ok(daemon::SchedulerStatus::Started(pid)) => {
                    format!("已自动启动调度服务（pid={pid}）。")
                }
                Ok(daemon::SchedulerStatus::Reloaded(pid)) => {
                    format!("已重载调度服务以应用新任务（pid={pid}）。")
                }
                Err(err) => format!(
                    "警告：任务已创建，但自动启动调度服务失败：{err}。请手动执行 `goldagent serve`。"
                ),
            };
            Ok(format!(
                "已自动创建定时任务：{} | {} | {} | retry={} | {}\n{}",
                job.id, job.name, job.schedule, job.retry_max, job.command, scheduler_note
            ))
        }
        ChatLocalAction::CronList => {
            let jobs = jobs::load_jobs(paths)?;
            if jobs.is_empty() {
                return Ok("当前没有定时任务。".to_string());
            }
            let mut lines = vec!["当前定时任务：".to_string()];
            for job in jobs {
                lines.push(format!(
                    "- {} | {} | {} | retry={} | {}",
                    job.id, job.name, job.schedule, job.retry_max, job.command
                ));
            }
            Ok(lines.join("\n"))
        }
        ChatLocalAction::CronRemove { id } => {
            let removed = jobs::remove_job(paths, &id)?;
            if removed {
                Ok(format!("已自动删除定时任务：{id}"))
            } else {
                Ok(format!("未找到定时任务：{id}"))
            }
        }
        ChatLocalAction::HookAddGit {
            repo,
            task,
            reference,
            interval_secs,
            name,
            retry_max,
            rules_file,
            report_file,
        } => {
            let command = if rules_file.is_some() {
                String::new()
            } else {
                build_scheduled_task_command(&task)
            };
            let hook = hooks::add_git_hook(
                paths,
                repo,
                reference,
                interval_secs,
                command,
                name,
                retry_max,
                rules_file,
                report_file,
            )?;
            let event = format!(
                "用户通过聊天创建了 hook：name={}，source={}，target={}，command={}",
                hook.name,
                hook.source.as_str(),
                hook.target,
                hook.command
            );
            memory::append_short_term(paths, "hook.add", &event)?;
            let _ = memory::auto_capture_event(paths, "hook.add", &event)?;
            let scheduler_note = match daemon::ensure_scheduler_running(paths) {
                Ok(daemon::SchedulerStatus::Started(pid)) => {
                    format!("已自动启动调度服务（pid={pid}）。")
                }
                Ok(daemon::SchedulerStatus::Reloaded(pid)) => {
                    format!("已重载调度服务以应用新任务（pid={pid}）。")
                }
                Err(err) => format!(
                    "警告：任务已创建，但自动启动调度服务失败：{err}。请手动执行 `goldagent serve`。"
                ),
            };
            let mode = if let Some(ref rf) = hook.rules_file {
                format!(
                    "LLM审查 rules={} report={}",
                    rf,
                    hook.report_file.as_deref().unwrap_or("<target>/goldagent-review.md")
                )
            } else {
                format!("command={}", hook.command)
            };
            Ok(format!(
                "已自动创建 Git hook：{} | {} | ref={} | interval={}s | retry={} | {}\n{}",
                hook.id,
                hook.name,
                hook.reference.as_deref().unwrap_or("HEAD"),
                hook.interval_secs,
                hook.retry_max,
                mode,
                scheduler_note
            ))
        }
        ChatLocalAction::HookAddP4 {
            depot,
            task,
            interval_secs,
            name,
            retry_max,
            rules_file,
            report_file,
        } => {
            let command = if rules_file.is_some() {
                String::new()
            } else {
                build_scheduled_task_command(&task)
            };
            let hook =
                hooks::add_p4_hook(paths, depot, interval_secs, command, name, retry_max, rules_file, report_file)?;
            let event = format!(
                "用户通过聊天创建了 hook：name={}，source={}，target={}，command={}",
                hook.name,
                hook.source.as_str(),
                hook.target,
                hook.command
            );
            memory::append_short_term(paths, "hook.add", &event)?;
            let _ = memory::auto_capture_event(paths, "hook.add", &event)?;
            let scheduler_note = match daemon::ensure_scheduler_running(paths) {
                Ok(daemon::SchedulerStatus::Started(pid)) => {
                    format!("已自动启动调度服务（pid={pid}）。")
                }
                Ok(daemon::SchedulerStatus::Reloaded(pid)) => {
                    format!("已重载调度服务以应用新任务（pid={pid}）。")
                }
                Err(err) => format!(
                    "警告：任务已创建，但自动启动调度服务失败：{err}。请手动执行 `goldagent serve`。"
                ),
            };
            let mode = if let Some(ref rf) = hook.rules_file {
                format!(
                    "LLM审查 rules={} report={}",
                    rf,
                    hook.report_file.as_deref().unwrap_or("<target>/goldagent-review.md")
                )
            } else {
                format!("command={}", hook.command)
            };
            Ok(format!(
                "已自动创建 P4 hook：{} | {} | interval={}s | retry={} | {}\n{}",
                hook.id,
                hook.name,
                hook.interval_secs,
                hook.retry_max,
                mode,
                scheduler_note
            ))
        }
        ChatLocalAction::HookList => {
            let hooks = hooks::load_hooks(paths)?;
            if hooks.is_empty() {
                return Ok("当前没有 hook 任务。".to_string());
            }
            let mut lines = vec!["当前 hook 任务：".to_string()];
            for hook in hooks {
                lines.push(format!(
                    "- {} | {} | {} | target={} | ref={} | interval={}s | retry={} | {}",
                    hook.id,
                    hook.name,
                    hook.source.as_str(),
                    hook.target,
                    hook.reference.as_deref().unwrap_or("-"),
                    hook.interval_secs,
                    hook.retry_max,
                    hook.command
                ));
            }
            Ok(lines.join("\n"))
        }
        ChatLocalAction::HookRemove { id } => {
            let removed = hooks::remove_hook(paths, &id)?;
            if removed {
                Ok(format!("已自动删除 hook：{id}"))
            } else {
                Ok(format!("未找到 hook：{id}"))
            }
        }
        ChatLocalAction::HookRulesNew { path } => {
            hooks::write_rules_template(&path)?;
            Ok(format!(
                "已生成审查规则模板：{path}\n编辑完成后告诉我仓库路径，我来帮你创建 hook。"
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ChatLocalAction, build_run_task_command, build_scheduled_task_command,
        extract_local_action_from_response,
    };

    #[test]
    fn parses_cron_add_action_line() {
        let raw = "[[LOCAL_ACTION:{\"kind\":\"cron_add\",\"schedule\":\"daily@13:00\",\"task\":\"提醒我吃饭\"}]]\n好的，已为你设置。";
        let (action, cleaned, err) = extract_local_action_from_response(raw);
        assert!(err.is_none());
        assert_eq!(
            action,
            Some(ChatLocalAction::CronAdd {
                schedule: "daily@13:00".to_string(),
                task: "提醒我吃饭".to_string(),
                name: None,
                retry_max: 1,
            })
        );
        assert_eq!(cleaned, "好的，已为你设置。");
    }

    #[test]
    fn parses_invalid_action_as_error() {
        let raw = "[[LOCAL_ACTION:{\"kind\":\"cron_add\"}]]\n参数不完整";
        let (action, cleaned, err) = extract_local_action_from_response(raw);
        assert!(action.is_none());
        assert_eq!(cleaned, "参数不完整");
        assert!(err.is_some());
    }

    #[test]
    fn escapes_run_task_command() {
        let out = build_run_task_command("提醒我说 \"hello\"");
        assert_eq!(out, "goldagent run \"提醒我说 \\\"hello\\\"\"");
    }

    #[test]
    fn uses_remind_command_for_reminder_task() {
        let out = build_scheduled_task_command("提醒我喝水");
        assert_eq!(out, "goldagent remind \"提醒我喝水\"");
    }
}

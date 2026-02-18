use crate::config::AgentPaths;
use crate::scheduler;
use anyhow::{Context, Result, anyhow};
use std::fs::OpenOptions;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchedulerStatus {
    Started(u32),
    Reloaded(u32),
}

pub fn ensure_scheduler_running(paths: &AgentPaths) -> Result<SchedulerStatus> {
    if let Some(pid) = scheduler::running_pid(paths)? {
        terminate_scheduler_process(pid)?;
        wait_until_stopped(paths)?;
        spawn_scheduler_process(paths)?;
        let new_pid = wait_until_started(paths)?;
        return Ok(SchedulerStatus::Reloaded(new_pid));
    }

    spawn_scheduler_process(paths)?;
    let pid = wait_until_started(paths)?;
    Ok(SchedulerStatus::Started(pid))
}

fn spawn_scheduler_process(paths: &AgentPaths) -> Result<()> {
    let exe = std::env::current_exe().context("unable to resolve current executable path")?;
    let log_path = paths.logs_dir.join("scheduler.log");
    let stdout = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("failed to open scheduler log at {}", log_path.display()))?;
    let stderr = stdout.try_clone()?;

    let mut cmd = Command::new(exe);
    cmd.arg("serve")
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }

    cmd.spawn()
        .context("failed to spawn scheduler background process")?;
    Ok(())
}

fn wait_until_started(paths: &AgentPaths) -> Result<u32> {
    for _ in 0..40 {
        if let Some(pid) = scheduler::running_pid(paths)? {
            return Ok(pid);
        }
        thread::sleep(Duration::from_millis(100));
    }
    Err(anyhow!(
        "scheduler process start requested, but pid file was not detected"
    ))
}

fn wait_until_stopped(paths: &AgentPaths) -> Result<()> {
    for _ in 0..40 {
        if scheduler::running_pid(paths)?.is_none() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(100));
    }
    Err(anyhow!(
        "scheduler stop requested, but existing process is still running"
    ))
}

#[cfg(unix)]
fn terminate_scheduler_process(pid: u32) -> Result<()> {
    let rc = unsafe { libc::kill(pid as i32, libc::SIGTERM) };
    if rc == 0 {
        Ok(())
    } else {
        Err(anyhow!(
            "failed to stop existing scheduler process {pid}: {}",
            std::io::Error::last_os_error()
        ))
    }
}

#[cfg(not(unix))]
fn terminate_scheduler_process(_pid: u32) -> Result<()> {
    Ok(())
}

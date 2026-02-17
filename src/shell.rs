use anyhow::{bail, Result};
use tokio::process::Command;

#[derive(Debug, Clone)]
pub struct ShellOutput {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

pub async fn run_shell_command(command: &str, force: bool) -> Result<ShellOutput> {
    if is_dangerous(command) && !force {
        bail!(
            "Blocked potentially dangerous command. Re-run with --force if this is intentional."
        );
    }

    let output = Command::new("zsh")
        .arg("-lc")
        .arg(command)
        .output()
        .await?;

    let exit_code = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    if !output.status.success() {
        bail!(
            "Command failed with code {exit_code}.\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
    }

    Ok(ShellOutput {
        exit_code,
        stdout,
        stderr,
    })
}

fn is_dangerous(command: &str) -> bool {
    let lowered = command.to_lowercase();
    [
        "rm -rf /",
        "mkfs",
        "shutdown",
        "reboot",
        "dd if=",
        ":(){:|:&};:",
    ]
    .iter()
    .any(|pattern| lowered.contains(pattern))
}

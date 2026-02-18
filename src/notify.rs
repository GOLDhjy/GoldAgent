use std::process::Command;

pub fn send_notification(title: &str, message: &str) -> bool {
    #[cfg(target_os = "macos")]
    {
        let script = format!(
            "display notification {} with title {}",
            apple_script_string(message),
            apple_script_string(title)
        );
        return Command::new("osascript")
            .arg("-e")
            .arg(script)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
    }

    #[cfg(target_os = "linux")]
    {
        return Command::new("notify-send")
            .arg(title)
            .arg(message)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (title, message);
        false
    }
}

#[cfg(target_os = "macos")]
fn apple_script_string(input: &str) -> String {
    format!("\"{}\"", input.replace('\\', "\\\\").replace('\"', "\\\""))
}

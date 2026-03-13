// notify.rs — Desktop notification support (v0.10.18.1).
//
// Sends system notifications when drafts are ready for review,
// so users don't have to watch the terminal.

use ta_submit::config::NotifyConfig;

/// Send a desktop notification for a draft ready for review.
///
/// On macOS, uses `osascript` to invoke Notification Center.
/// On Linux, uses `notify-send` if available.
/// Failures are logged but never block the workflow.
pub fn draft_ready(config: &NotifyConfig, goal_title: &str, draft_display_id: &str) {
    if !config.enabled {
        return;
    }

    let title = format!("{}: Draft ready", config.title);
    let body = format!("{}\n\nRun: ta draft view {}", goal_title, draft_display_id);

    if let Err(e) = send_notification(&title, &body) {
        tracing::debug!(error = %e, "Desktop notification failed (non-fatal)");
    }
}

/// Send a desktop notification for verification failure.
pub fn verification_failed(config: &NotifyConfig, failed_count: usize, total_count: usize) {
    if !config.enabled {
        return;
    }

    let title = format!("{}: Verification failed", config.title);
    let body = format!(
        "{} of {} checks failed.\nRun: ta verify",
        failed_count, total_count
    );

    if let Err(e) = send_notification(&title, &body) {
        tracing::debug!(error = %e, "Desktop notification failed (non-fatal)");
    }
}

/// Platform-specific notification dispatch.
fn send_notification(title: &str, body: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        send_macos_notification(title, body)
    }
    #[cfg(target_os = "linux")]
    {
        send_linux_notification(title, body)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (title, body);
        Err("Desktop notifications not supported on this platform".to_string())
    }
}

#[cfg(target_os = "macos")]
fn send_macos_notification(title: &str, body: &str) -> Result<(), String> {
    use std::process::Command;

    // Use osascript to send a notification via Notification Center.
    // This is the most reliable approach on macOS without external dependencies.
    let script = format!(
        "display notification \"{}\" with title \"{}\"",
        body.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n"),
        title.replace('\\', "\\\\").replace('"', "\\\""),
    );

    let output = Command::new("osascript")
        .arg("-e")
        .arg(&script)
        .output()
        .map_err(|e| format!("Failed to run osascript: {}", e))?;

    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!("osascript failed: {}", stderr.trim()))
    }
}

#[cfg(target_os = "linux")]
fn send_linux_notification(title: &str, body: &str) -> Result<(), String> {
    use std::process::Command;

    let output = Command::new("notify-send")
        .arg("--app-name=ta")
        .arg(title)
        .arg(body)
        .output()
        .map_err(|e| format!("Failed to run notify-send: {}", e))?;

    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!("notify-send failed: {}", stderr.trim()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_config_does_not_send() {
        let config = NotifyConfig {
            enabled: false,
            title: "TA".to_string(),
        };
        // Should return immediately without trying to send.
        draft_ready(&config, "Test goal", "abc123");
        verification_failed(&config, 1, 3);
    }

    #[test]
    fn default_config_is_enabled() {
        let config = NotifyConfig::default();
        assert!(config.enabled);
        assert_eq!(config.title, "TA");
    }
}

use anyhow::Context;
use tokio::process::Command;

/// Returns `true` if the current process is running as root (UID 0) on Unix.
#[cfg(unix)]
async fn is_root() -> bool {
    Command::new("id")
        .arg("-u")
        .output()
        .await
        .ok()
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .and_then(|s| s.trim().parse::<u32>().ok())
        .map(|uid| uid == 0)
        .unwrap_or(false)
}

/// Runs `program args` with elevated privileges.
///
/// On Linux: uses `pkexec` for a GUI auth dialog when in a graphical session, otherwise `sudo`.
/// On MacOS: uses `osascript` to show a GUI dialog.
/// On Windows: runs the command directly (the caller must hold administrator rights).
async fn privileged(program: &str, args: &[&str]) -> anyhow::Result<std::process::ExitStatus> {
    #[cfg(target_os = "linux")]
    {
        if is_root().await {
            Command::new(program)
                .args(args)
                .status()
                .await
                .with_context(|| format!("failed to run `{program}`"))
        } else if std::env::var("DISPLAY").is_ok() || std::env::var("WAYLAND_DISPLAY").is_ok() {
            // Graphical session: use pkexec for a GUI auth dialog
            Command::new("pkexec")
                .arg(program)
                .args(args)
                .status()
                .await
                .with_context(|| format!("failed to run `pkexec {program}`"))
        } else {
            // Headless/terminal: fall back to sudo
            Command::new("sudo")
                .arg(program)
                .args(args)
                .status()
                .await
                .with_context(|| format!("failed to run `sudo {program}`"))
        }
    }

    #[cfg(target_os = "macos")]
    {
        if is_root().await {
            Command::new(program)
                .args(args)
                .status()
                .await
                .with_context(|| format!("failed to run `{program}`"))
        } else {
            // Build a quoted shell command string for osascript
            let args_str = args
                .iter()
                .map(|a| shell_escape(a))
                .collect::<Vec<_>>()
                .join(" ");
            let shell_cmd = if args_str.is_empty() {
                shell_escape(program)
            } else {
                format!("{} {}", shell_escape(program), args_str)
            };

            let script = format!(r#"do shell script "{shell_cmd}" with administrator privileges"#);

            Command::new("osascript")
                .arg("-e")
                .arg(&script)
                .status()
                .await
                .with_context(|| format!("failed to run `{program}` via osascript"))
        }
    }

    #[cfg(windows)]
    {
        Command::new(program)
            .args(args)
            .status()
            .await
            .with_context(|| format!("failed to run `{program}`"))
    }
}

/// Minimal shell-escaping for macOS osascript: wraps in quotes and escapes
/// backslashes, double-quotes, and backticks.
#[cfg(target_os = "macos")]
fn shell_escape(s: &str) -> String {
    let escaped = s
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('`', "\\`");
    format!("\"{escaped}\"")
}

/// Manages the `cloudflared` system service.
pub struct CloudflaredService;

impl CloudflaredService {
    /// Returns `true` if the `cloudflared` binary is available on PATH.
    pub async fn is_installed() -> bool {
        Command::new("cloudflared")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// Returns `true` if the cloudflared system service is currently running.
    pub async fn is_running() -> bool {
        #[cfg(target_os = "linux")]
        {
            Command::new("systemctl")
                .args(["is-active", "--quiet", "cloudflared"])
                .status()
                .await
                .map(|s| s.success())
                .unwrap_or(false)
        }

        #[cfg(target_os = "macos")]
        {
            Command::new("launchctl")
                .args(["list"])
                .output()
                .await
                .map_or(false, |out| {
                    String::from_utf8_lossy(&out.stdout).contains("cloudflared")
                })
        }

        #[cfg(target_os = "windows")]
        {
            Command::new("Get-Service")
                .args(["-Name", "Cloudflared"])
                .output()
                .await
                .map_or(false, |out| {
                    String::from_utf8_lossy(&out.stdout).contains("Running")
                })
        }

        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        {
            false
        }
    }

    /// Installs and starts cloudflared as a system service using the provided token.
    ///
    /// Runs `cloudflared service install <token>` with elevated privileges.
    pub async fn install_and_start(token: &str) -> anyhow::Result<()> {
        let status = privileged("cloudflared", &["service", "install", token]).await?;

        if !status.success() {
            anyhow::bail!(
                "`cloudflared service install` exited with status {}",
                status
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// is_installed() must not panic; result depends on the test environment.
    #[tokio::test]
    async fn test_is_installed_does_not_panic() {
        let _ = CloudflaredService::is_installed().await;
    }

    /// is_running() must not panic; result depends on the test environment.
    #[tokio::test]
    async fn test_is_running_does_not_panic() {
        let _ = CloudflaredService::is_running().await;
    }
}

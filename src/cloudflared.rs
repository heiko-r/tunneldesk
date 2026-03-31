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
/// On Unix: prepends `sudo` when the current process is not already root.
/// On Windows: runs the command directly (the caller must hold administrator rights).
async fn privileged(program: &str, args: &[&str]) -> anyhow::Result<std::process::ExitStatus> {
    #[cfg(unix)]
    {
        if is_root().await {
            Command::new(program)
                .args(args)
                .status()
                .await
                .with_context(|| format!("failed to run `{program}`"))
        } else {
            Command::new("sudo")
                .arg(program)
                .args(args)
                .status()
                .await
                .with_context(|| format!("failed to run `sudo {program}`"))
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
            let out = Command::new("launchctl")
                .args(["list"])
                .output()
                .await
                .unwrap_or_default();
            String::from_utf8_lossy(&out.stdout).contains("cloudflared")
        }

        #[cfg(target_os = "windows")]
        {
            let out = Command::new("sc")
                .args(["query", "cloudflared"])
                .output()
                .await
                .unwrap_or_default();
            String::from_utf8_lossy(&out.stdout).contains("RUNNING")
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

    /// Restarts the cloudflared system service.
    pub async fn restart() -> anyhow::Result<()> {
        #[cfg(target_os = "linux")]
        {
            let status = privileged("systemctl", &["restart", "cloudflared"]).await?;

            if !status.success() {
                anyhow::bail!(
                    "`systemctl restart cloudflared` failed with status {}",
                    status
                );
            }
            Ok(())
        }

        #[cfg(target_os = "macos")]
        {
            // On macOS, stop then start the launchd service.
            privileged("launchctl", &["stop", "com.cloudflare.cloudflared"])
                .await
                .ok();
            let status = privileged("launchctl", &["start", "com.cloudflare.cloudflared"]).await?;
            if !status.success() {
                anyhow::bail!("launchctl start cloudflared failed with status {}", status);
            }
            Ok(())
        }

        #[cfg(target_os = "windows")]
        {
            privileged("sc", &["stop", "cloudflared"]).await.ok();
            let status = privileged("sc", &["start", "cloudflared"]).await?;
            if !status.success() {
                anyhow::bail!("sc start cloudflared failed with status {}", status);
            }
            Ok(())
        }

        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        {
            anyhow::bail!("restart not supported on this platform");
        }
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

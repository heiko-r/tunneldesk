use std::process::Stdio;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, watch};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// Default program name of the Cloudflare tunnel connector.
pub const CLOUDFLARED_PROGRAM: &str = "cloudflared";

const INITIAL_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(30);
/// A connector that ran at least this long is considered healthy, so the
/// restart backoff starts over after it exits.
const HEALTHY_RUN: Duration = Duration::from_secs(60);
const GRACEFUL_STOP_TIMEOUT: Duration = Duration::from_secs(5);

/// State of the `cloudflared` connector as seen by the core.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConnectorState {
    /// No connector is running (not configured, exited, or shut down).
    Stopped,
    /// The connector process was spawned but has not registered a connection yet.
    Starting,
    /// At least one tunnel connection is registered with the Cloudflare edge.
    Connected,
    /// `manage_cloudflared = false`: the user runs the connector themselves.
    External,
    /// The `cloudflared` binary was not found on `PATH`.
    NotInstalled,
}

/// Maps a `cloudflared` log line to the connector state it implies, if any.
pub fn state_from_log_line(line: &str) -> Option<ConnectorState> {
    if line.contains("Registered tunnel connection") {
        Some(ConnectorState::Connected)
    } else {
        None
    }
}

/// Computes the delay before the next restart attempt.
pub fn next_backoff(current: Duration, ran_for: Duration) -> Duration {
    if ran_for >= HEALTHY_RUN {
        INITIAL_BACKOFF
    } else {
        (current * 2).min(MAX_BACKOFF)
    }
}

/// A supervised `cloudflared tunnel run` child process.
///
/// The process is restarted with exponential backoff when it exits
/// unexpectedly, and stopped gracefully by [`shutdown`](Self::shutdown).
pub struct CloudflaredProcess {
    cancel: CancellationToken,
    supervisor: Mutex<Option<JoinHandle<()>>>,
}

impl CloudflaredProcess {
    /// Spawns `cloudflared` for the tunnel identified by `token`.
    pub fn spawn(token: String, state: watch::Sender<ConnectorState>) -> Self {
        Self::spawn_program(CLOUDFLARED_PROGRAM.into(), token, state)
    }

    /// Spawns `program` (a `cloudflared`-compatible executable) for `token`.
    pub fn spawn_program(
        program: String,
        token: String,
        state: watch::Sender<ConnectorState>,
    ) -> Self {
        let cancel = CancellationToken::new();
        let supervisor = tokio::spawn(supervise(program, token, state, cancel.clone()));
        Self {
            cancel,
            supervisor: Mutex::new(Some(supervisor)),
        }
    }

    /// Stops the connector: SIGTERM first, SIGKILL after a timeout.
    pub async fn shutdown(&self) {
        self.cancel.cancel();
        if let Some(handle) = self.supervisor.lock().await.take() {
            handle.await.ok();
        }
    }
}

impl Drop for CloudflaredProcess {
    fn drop(&mut self) {
        // Lets the supervisor terminate the child even without an explicit shutdown.
        self.cancel.cancel();
    }
}

async fn supervise(
    program: String,
    token: String,
    state: watch::Sender<ConnectorState>,
    cancel: CancellationToken,
) {
    let mut backoff = INITIAL_BACKOFF;
    loop {
        state.send_replace(ConnectorState::Starting);
        let started = tokio::time::Instant::now();

        match spawn_child(&program, &token) {
            Ok(mut child) => {
                if let Some(stderr) = child.stderr.take() {
                    tokio::spawn(forward_logs(stderr, state.clone()));
                }
                tokio::select! {
                    status = child.wait() => {
                        match status {
                            Ok(s) => tracing::warn!("cloudflared exited with {s}"),
                            Err(e) => tracing::warn!("Failed to wait for cloudflared: {e}"),
                        }
                        state.send_replace(ConnectorState::Stopped);
                    }
                    _ = cancel.cancelled() => {
                        terminate(&mut child).await;
                        state.send_replace(ConnectorState::Stopped);
                        return;
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::warn!(
                    "`{program}` not found on PATH; install cloudflared to connect the tunnel"
                );
                state.send_replace(ConnectorState::NotInstalled);
            }
            Err(e) => {
                tracing::warn!("Failed to spawn `{program}`: {e}");
                state.send_replace(ConnectorState::Stopped);
            }
        }

        backoff = next_backoff(backoff, started.elapsed());
        tokio::select! {
            _ = tokio::time::sleep(backoff) => {}
            _ = cancel.cancelled() => return,
        }
    }
}

fn spawn_child(program: &str, token: &str) -> std::io::Result<Child> {
    let mut cmd = Command::new(program);
    cmd.args([
        "tunnel",
        "--no-autoupdate",
        "--metrics",
        "127.0.0.1:0",
        "run",
    ])
    // Passing the token via the environment keeps it out of `ps` output.
    .env("TUNNEL_TOKEN", token)
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::piped())
    .kill_on_drop(true);
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd.spawn()
}

async fn forward_logs(stderr: tokio::process::ChildStderr, state: watch::Sender<ConnectorState>) {
    let mut lines = BufReader::new(stderr).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if line.contains(" ERR ") {
            tracing::warn!(target: "cloudflared", "{line}");
        } else {
            tracing::info!(target: "cloudflared", "{line}");
        }
        if let Some(new_state) = state_from_log_line(&line) {
            state.send_replace(new_state);
        }
    }
}

async fn terminate(child: &mut Child) {
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        // SAFETY: `kill` has no memory-safety preconditions; `pid` belongs to our child.
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGTERM);
        }
        if tokio::time::timeout(GRACEFUL_STOP_TIMEOUT, child.wait())
            .await
            .is_ok()
        {
            return;
        }
    }
    let _ = child.kill().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registered_connection_line_means_connected() {
        let line = "2026-09-25T10:00:00Z INF Registered tunnel connection connIndex=0 \
                    connection=abc event=0 ip=198.41.200.13 location=fra08 protocol=quic";
        assert_eq!(state_from_log_line(line), Some(ConnectorState::Connected));
    }

    #[test]
    fn unrelated_line_does_not_change_state() {
        assert_eq!(
            state_from_log_line("2026-09-25T10:00:00Z INF Starting tunnel tunnelID=abc"),
            None
        );
    }

    #[test]
    fn backoff_doubles_up_to_max() {
        let short = Duration::from_secs(1);
        assert_eq!(
            next_backoff(Duration::from_secs(1), short),
            Duration::from_secs(2)
        );
        assert_eq!(
            next_backoff(Duration::from_secs(16), short),
            Duration::from_secs(30)
        );
        assert_eq!(next_backoff(MAX_BACKOFF, short), MAX_BACKOFF);
    }

    #[test]
    fn backoff_resets_after_healthy_run() {
        assert_eq!(
            next_backoff(Duration::from_secs(30), HEALTHY_RUN),
            INITIAL_BACKOFF
        );
    }

    #[test]
    fn connector_state_serializes_as_variant_name() {
        assert_eq!(
            serde_json::to_string(&ConnectorState::Connected).unwrap(),
            "\"Connected\""
        );
    }

    #[tokio::test]
    async fn missing_program_reports_not_installed() {
        let (tx, mut rx) = watch::channel(ConnectorState::Stopped);
        let process = CloudflaredProcess::spawn_program(
            "/nonexistent/cloudflared-for-tests".into(),
            "token".into(),
            tx,
        );
        tokio::time::timeout(
            Duration::from_secs(5),
            rx.wait_for(|s| *s == ConnectorState::NotInstalled),
        )
        .await
        .expect("state change timed out")
        .unwrap();
        process.shutdown().await;
    }

    #[cfg(unix)]
    fn fake_cloudflared(dir: &std::path::Path, body: &str) -> String {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("cloudflared");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path.to_string_lossy().into_owned()
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fake_connector_reaches_connected_and_stops_on_shutdown() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("token.txt");
        let program = fake_cloudflared(
            dir.path(),
            &format!(
                "echo \"$TUNNEL_TOKEN\" > '{}'\n\
                 echo 'INF Registered tunnel connection connIndex=0' >&2\n\
                 trap 'exit 0' TERM\n\
                 while true; do sleep 0.1; done",
                marker.display()
            ),
        );
        let (tx, mut rx) = watch::channel(ConnectorState::Stopped);
        let process = CloudflaredProcess::spawn_program(program, "secret-token".into(), tx);

        tokio::time::timeout(
            Duration::from_secs(5),
            rx.wait_for(|s| *s == ConnectorState::Connected),
        )
        .await
        .expect("connector never connected")
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(&marker).unwrap().trim(),
            "secret-token"
        );

        tokio::time::timeout(Duration::from_secs(10), process.shutdown())
            .await
            .expect("shutdown hung");
        assert_eq!(*rx.borrow(), ConnectorState::Stopped);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn exited_connector_is_restarted() {
        let dir = tempfile::tempdir().unwrap();
        let counter = dir.path().join("runs");
        let program = fake_cloudflared(
            dir.path(),
            &format!("echo run >> '{}'\nexit 1", counter.display()),
        );
        let (tx, _rx) = watch::channel(ConnectorState::Stopped);
        let process = CloudflaredProcess::spawn_program(program, "t".into(), tx);

        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            let runs = std::fs::read_to_string(&counter)
                .map(|s| s.lines().count())
                .unwrap_or(0);
            if runs >= 2 {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "connector was not restarted"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        process.shutdown().await;
    }
}

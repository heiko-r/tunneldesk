//! Helpers for integration tests that drive the real `tunneldesk` binary.
//!
//! Every [`TestEnv`] has its own runtime directory (via `TUNNELDESK_RUNTIME_DIR`)
//! so tests never see each other's cores, nor a core the developer is running.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use serde::Deserialize;
use tokio::process::{Child, Command};

pub const BIN: &str = env!("CARGO_BIN_EXE_tunneldesk");
pub const RUNTIME_DIR_ENV: &str = "TUNNELDESK_RUNTIME_DIR";
pub const EXIT_ALREADY_RUNNING: i32 = 3;

/// Contents of the `<hash>.json` info file a running core publishes.
#[derive(Debug, Clone, Deserialize)]
pub struct CoreInfo {
    pub pid: u32,
    pub port: u16,
    pub version: String,
    pub config_path: PathBuf,
    pub config_hash: String,
}

impl CoreInfo {
    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
}

pub struct TestEnv {
    pub dir: tempfile::TempDir,
}

impl TestEnv {
    pub fn new() -> Self {
        Self {
            dir: tempfile::tempdir().unwrap(),
        }
    }

    pub fn runtime_dir(&self) -> PathBuf {
        self.dir.path().join("run")
    }

    /// Writes a config with an ephemeral port and a short idle timeout,
    /// followed by `extra` TOML.
    pub fn write_config(&self, name: &str, extra: &str) -> PathBuf {
        let path = self.dir.path().join(name);
        let body = format!("[gui]\nport = 0\n\n[core]\nidle_timeout_secs = 1\n\n{extra}");
        std::fs::write(&path, body).unwrap();
        std::fs::canonicalize(path).unwrap()
    }

    /// A command for the binary with this environment's runtime dir and `config`.
    pub fn command(&self, config: &Path, args: &[&str]) -> Command {
        let mut cmd = Command::new(BIN);
        cmd.env(RUNTIME_DIR_ENV, self.runtime_dir())
            .arg("--config")
            .arg(config)
            .args(args)
            .stdin(Stdio::null())
            .kill_on_drop(true);
        cmd
    }

    /// Runs a foreground core (`serve`) with its output captured in a log file.
    pub fn spawn_serve(&self, config: &Path) -> Child {
        let log = std::fs::File::create(config.with_extension("serve.log")).unwrap();
        self.command(config, &["serve"])
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .unwrap()
    }

    /// Runs a command to completion and returns (status, stdout, stderr).
    pub async fn run(&self, config: &Path, args: &[&str]) -> (ExitStatus, String, String) {
        let output =
            tokio::time::timeout(Duration::from_secs(30), self.command(config, args).output())
                .await
                .unwrap_or_else(|_| panic!("`tunneldesk {}` timed out", args.join(" ")))
                .unwrap();
        (
            output.status,
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    }

    /// Info files of all cores currently published in this environment.
    pub fn published_cores(&self) -> Vec<CoreInfo> {
        let Ok(entries) = std::fs::read_dir(self.runtime_dir()) else {
            return Vec::new();
        };
        entries
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
            .filter_map(|e| std::fs::read(e.path()).ok())
            .filter_map(|bytes| serde_json::from_slice(&bytes).ok())
            .collect()
    }

    pub fn find_info(&self, config: &Path) -> Option<CoreInfo> {
        self.published_cores()
            .into_iter()
            .find(|info| info.config_path == config)
    }

    /// Waits until a core for `config` has published its info file.
    pub async fn wait_for_info(&self, config: &Path) -> CoreInfo {
        wait_until(Duration::from_secs(20), || self.find_info(config))
            .await
            .unwrap_or_else(|| {
                let log =
                    std::fs::read_to_string(config.with_extension("serve.log")).unwrap_or_default();
                panic!(
                    "no core published an info file for {}\n{log}",
                    config.display()
                )
            })
    }

    /// Waits until no core for `config` is published any more.
    pub async fn wait_for_info_removed(&self, config: &Path) -> bool {
        wait_until(Duration::from_secs(20), || {
            self.find_info(config).is_none().then_some(())
        })
        .await
        .is_some()
    }
}

impl Drop for TestEnv {
    /// Detached cores are not children of the test; make sure none outlive it.
    fn drop(&mut self) {
        #[cfg(unix)]
        for info in self.published_cores() {
            let _ = std::process::Command::new("kill")
                .arg(info.pid.to_string())
                .status();
        }
    }
}

/// Polls `check` every 50 ms until it returns `Some` or `timeout` passes.
pub async fn wait_until<T>(timeout: Duration, mut check: impl FnMut() -> Option<T>) -> Option<T> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Some(value) = check() {
            return Some(value);
        }
        if tokio::time::Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Waits for `child` to exit, returning `None` on timeout.
pub async fn wait_for_exit(child: &mut Child, timeout: Duration) -> Option<ExitStatus> {
    tokio::time::timeout(timeout, child.wait())
        .await
        .ok()
        .map(Result::unwrap)
}

/// The `[gui] access_token` the core generated into `config`.
pub fn read_token(config: &Path) -> String {
    let text = std::fs::read_to_string(config).unwrap();
    let value: toml::Value = toml::from_str(&text).unwrap();
    value["gui"]["access_token"]
        .as_str()
        .expect("core should have generated gui.access_token")
        .to_string()
}

pub fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap()
}

/// Whether a process with `pid` is still alive.
#[cfg(unix)]
pub fn process_alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

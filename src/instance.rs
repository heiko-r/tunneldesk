//! Discovery of the core process that belongs to a config file.
//!
//! Each core is keyed by a hash of its canonical config path. In the runtime
//! directory it holds an exclusive lock on `<hash>.lock` for its lifetime
//! and publishes `<hash>.json` ([`CoreInfo`]) once it is listening, so any
//! frontend can find it, or start it if none is running.

use std::fs::{File, TryLockError};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::web_server::HealthResponse;

/// Overrides the runtime directory (used by tests to isolate instances).
pub const RUNTIME_DIR_ENV: &str = "TUNNELDESK_RUNTIME_DIR";
/// Exit code of a core that found another core already running for its config.
pub const EXIT_ALREADY_RUNNING: i32 = 3;

const START_TIMEOUT: Duration = Duration::from_secs(15);
const POLL_INTERVAL: Duration = Duration::from_millis(100);
const HEALTH_TIMEOUT: Duration = Duration::from_secs(2);

/// Published by a running core in `<hash>.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

/// Directory holding lock, info and log files of all cores for this user.
pub fn runtime_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os(RUNTIME_DIR_ENV) {
        return PathBuf::from(dir);
    }
    #[cfg(target_os = "linux")]
    let base = dirs::runtime_dir()
        .map(|d| d.join("tunneldesk"))
        .or_else(|| dirs::state_dir().map(|d| d.join("tunneldesk")));
    #[cfg(not(target_os = "linux"))]
    let base = dirs::data_local_dir().map(|d| d.join("TunnelDesk").join("run"));
    base.unwrap_or_else(|| std::env::temp_dir().join("tunneldesk"))
}

/// Resolves `path` to an absolute, canonical path, even if the file does not
/// exist yet (its parent directory is created and canonicalized instead).
pub fn canonical_config_path(path: &Path) -> std::io::Result<PathBuf> {
    if path.exists() {
        return path.canonicalize();
    }
    let parent = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    };
    std::fs::create_dir_all(&parent)?;
    let file_name = path.file_name().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "config path has no file name",
        )
    })?;
    Ok(parent.canonicalize()?.join(file_name))
}

/// Stable identifier of a canonical config path.
pub fn config_hash(canonical: &Path) -> String {
    let digest = Sha256::digest(canonical.to_string_lossy().as_bytes());
    digest[..8].iter().map(|b| format!("{b:02x}")).collect()
}

/// File locations of one core in the runtime directory.
#[derive(Debug, Clone)]
pub struct InstancePaths {
    pub lock: PathBuf,
    pub info: PathBuf,
    pub log: PathBuf,
}

impl InstancePaths {
    pub fn for_hash(hash: &str) -> Self {
        Self::in_dir(&runtime_dir(), hash)
    }

    fn in_dir(dir: &Path, hash: &str) -> Self {
        Self {
            lock: dir.join(format!("{hash}.lock")),
            info: dir.join(format!("{hash}.json")),
            log: dir.join(format!("{hash}.log")),
        }
    }
}

/// Exclusive per-config lock held for the lifetime of a core. The OS releases
/// it when the process exits, so a crashed core never leaves a stale lock.
#[derive(Debug)]
pub struct CoreLock {
    _file: File,
}

/// Why a [`CoreLock`] could not be acquired.
#[derive(Debug)]
pub enum LockError {
    AlreadyLocked,
    Io(std::io::Error),
}

impl std::fmt::Display for LockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LockError::AlreadyLocked => write!(f, "another core holds the lock"),
            LockError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for LockError {}

impl CoreLock {
    pub fn acquire(paths: &InstancePaths) -> Result<Self, LockError> {
        if let Some(dir) = paths.lock.parent() {
            create_private_dir(dir).map_err(LockError::Io)?;
        }
        // The lock file is never deleted: Windows cannot delete a locked file,
        // and deleting it on Unix would let two cores lock different inodes.
        let file = File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&paths.lock)
            .map_err(LockError::Io)?;
        match file.try_lock() {
            Ok(()) => Ok(Self { _file: file }),
            Err(TryLockError::WouldBlock) => Err(LockError::AlreadyLocked),
            Err(TryLockError::Error(e)) => Err(LockError::Io(e)),
        }
    }
}

fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Atomically writes the info file (temp file + rename, owner-only permissions).
pub fn write_info(paths: &InstancePaths, info: &CoreInfo) -> std::io::Result<()> {
    let tmp = paths.info.with_extension("json.tmp");
    {
        let mut options = File::options();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&tmp)?;
        file.write_all(&serde_json::to_vec_pretty(info)?)?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, &paths.info)
}

pub fn read_info(paths: &InstancePaths) -> Option<CoreInfo> {
    let bytes = std::fs::read(&paths.info).ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub fn remove_info(paths: &InstancePaths) {
    let _ = std::fs::remove_file(&paths.info);
}

fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .timeout(HEALTH_TIMEOUT)
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

/// Returns the health response if a core for `info.config_hash` answers on `info.port`.
pub async fn check_health(info: &CoreInfo) -> Option<HealthResponse> {
    let health: HealthResponse = http_client()
        .get(format!(
            "{}{}",
            info.base_url(),
            crate::security::HEALTH_PATH
        ))
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    (health.config_hash == info.config_hash).then_some(health)
}

/// Returns the running core for `canonical`, if any.
pub async fn find_core(canonical: &Path) -> Option<CoreInfo> {
    let paths = InstancePaths::for_hash(&config_hash(canonical));
    let info = read_info(&paths)?;
    let health = check_health(&info).await?;
    if health.version != env!("CARGO_PKG_VERSION") {
        tracing::warn!(
            "Attaching to a TunnelDesk core of version {} (this is {}); \
             stop it with `tunneldesk stop` to upgrade",
            health.version,
            env!("CARGO_PKG_VERSION")
        );
    }
    Some(info)
}

/// Returns the core for `config_path`, starting a background core if none runs.
pub async fn ensure_core(config_path: &Path) -> anyhow::Result<CoreInfo> {
    let canonical = canonical_config_path(config_path)
        .with_context(|| format!("invalid config path {}", config_path.display()))?;
    if let Some(info) = find_core(&canonical).await {
        return Ok(info);
    }

    let paths = InstancePaths::for_hash(&config_hash(&canonical));
    tracing::info!("Starting TunnelDesk core for {}", canonical.display());
    let mut child = spawn_detached(&canonical).context("failed to start the TunnelDesk core")?;

    let deadline = tokio::time::Instant::now() + START_TIMEOUT;
    let mut child_exited = false;
    while tokio::time::Instant::now() < deadline {
        if let Some(info) = find_core(&canonical).await {
            reap_in_background(child);
            return Ok(info);
        }
        if !child_exited && let Ok(Some(status)) = child.try_wait() {
            child_exited = true;
            // Losing a start race to another frontend is fine: keep polling
            // for the winner. Anything else is a startup failure.
            if status.code() != Some(EXIT_ALREADY_RUNNING) {
                anyhow::bail!(
                    "the TunnelDesk core exited during startup ({status}).{}",
                    log_hint(&paths)
                );
            }
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    if !child_exited {
        reap_in_background(child);
    }
    anyhow::bail!(
        "timed out waiting for the TunnelDesk core to start.{}",
        log_hint(&paths)
    )
}

fn log_hint(paths: &InstancePaths) -> String {
    let tail = std::fs::read_to_string(&paths.log)
        .map(|log| {
            let lines: Vec<&str> = log.lines().collect();
            lines[lines.len().saturating_sub(10)..].join("\n")
        })
        .unwrap_or_default();
    if tail.is_empty() {
        format!(" See {}", paths.log.display())
    } else {
        format!(" Last log lines from {}:\n{tail}", paths.log.display())
    }
}

/// Waits for the child in a background thread so it does not linger as a zombie.
fn reap_in_background(mut child: Child) {
    std::thread::spawn(move || {
        let _ = child.wait();
    });
}

/// Starts `tunneldesk core --detached` for `canonical`, detached from this
/// process so that it outlives the frontend that started it.
fn spawn_detached(canonical: &Path) -> std::io::Result<Child> {
    let exe = std::env::current_exe()?;
    let mut cmd = Command::new(exe);
    cmd.arg("--config")
        .arg(canonical)
        .args(["core", "--detached"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    cmd.spawn()
}

/// Lists all cores in the runtime directory that answer their health check.
pub async fn live_cores() -> Vec<CoreInfo> {
    let Ok(entries) = std::fs::read_dir(runtime_dir()) else {
        return Vec::new();
    };
    let mut cores = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let Some(info) = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice::<CoreInfo>(&b).ok())
        else {
            continue;
        };
        if check_health(&info).await.is_some() {
            cores.push(info);
        }
    }
    cores.sort_by(|a, b| a.config_path.cmp(&b.config_path));
    cores
}

/// Asks the core to shut down and waits until its info file disappears.
pub async fn stop_core(info: &CoreInfo, access_token: &str) -> anyhow::Result<()> {
    let response = http_client()
        .post(format!("{}/api/shutdown", info.base_url()))
        .bearer_auth(access_token)
        .send()
        .await
        .context("failed to reach the TunnelDesk core")?;
    if !response.status().is_success() {
        anyhow::bail!(
            "the core rejected the shutdown request: {}",
            response.status()
        );
    }
    let paths = InstancePaths::for_hash(&info.config_hash);
    let deadline = tokio::time::Instant::now() + START_TIMEOUT;
    while paths.info.exists() && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_info(port: u16) -> CoreInfo {
        CoreInfo {
            pid: 42,
            port,
            version: "1.2.3".into(),
            config_path: PathBuf::from("/tmp/config.toml"),
            config_hash: "abcdef0123456789".into(),
        }
    }

    #[test]
    fn config_hash_is_stable_and_distinct() {
        let a = config_hash(Path::new("/home/user/a.toml"));
        assert_eq!(a, config_hash(Path::new("/home/user/a.toml")));
        assert_ne!(a, config_hash(Path::new("/home/user/b.toml")));
        assert_eq!(a.len(), 16);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn canonical_path_resolves_existing_and_missing_files() {
        let dir = tempfile::tempdir().unwrap();
        let existing = dir.path().join("a.toml");
        std::fs::write(&existing, "").unwrap();
        let canonical = canonical_config_path(&existing).unwrap();
        assert!(canonical.is_absolute());
        assert_eq!(canonical, existing.canonicalize().unwrap());

        let missing = dir.path().join("sub/./b.toml");
        let canonical = canonical_config_path(&missing).unwrap();
        assert_eq!(
            canonical,
            dir.path().canonicalize().unwrap().join("sub/b.toml")
        );
        assert!(dir.path().join("sub").is_dir(), "parent must be created");
    }

    #[test]
    fn info_file_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let paths = InstancePaths::in_dir(dir.path(), "abc");
        assert!(read_info(&paths).is_none());
        write_info(&paths, &sample_info(1234)).unwrap();
        assert_eq!(read_info(&paths), Some(sample_info(1234)));
        remove_info(&paths);
        assert!(read_info(&paths).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn info_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let paths = InstancePaths::in_dir(dir.path(), "abc");
        write_info(&paths, &sample_info(1)).unwrap();
        let mode = std::fs::metadata(&paths.info).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn lock_is_exclusive_until_released() {
        let dir = tempfile::tempdir().unwrap();
        let paths = InstancePaths::in_dir(dir.path(), "abc");
        let first = CoreLock::acquire(&paths).unwrap();
        assert!(matches!(
            CoreLock::acquire(&paths),
            Err(LockError::AlreadyLocked)
        ));
        drop(first);
        CoreLock::acquire(&paths).expect("lock must be free after release");
    }

    #[test]
    fn locks_for_different_configs_are_independent() {
        let dir = tempfile::tempdir().unwrap();
        let _a = CoreLock::acquire(&InstancePaths::in_dir(dir.path(), "a")).unwrap();
        let _b = CoreLock::acquire(&InstancePaths::in_dir(dir.path(), "b")).unwrap();
    }

    #[tokio::test]
    async fn health_check_fails_without_server() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        assert!(check_health(&sample_info(port)).await.is_none());
    }

    #[tokio::test]
    async fn health_check_requires_matching_hash() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[gui]\nport = 0\n").unwrap();
        let canonical = canonical_config_path(&path).unwrap();
        let core = crate::core::Core::start(
            &canonical,
            crate::core::CoreOptions {
                idle_shutdown: false,
            },
        )
        .await
        .unwrap();

        let mut info = CoreInfo {
            pid: std::process::id(),
            port: core.port(),
            version: env!("CARGO_PKG_VERSION").into(),
            config_path: canonical.clone(),
            config_hash: config_hash(&canonical),
        };
        assert!(check_health(&info).await.is_some());
        info.config_hash = "0000000000000000".into();
        assert!(check_health(&info).await.is_none());
        core.shutdown().await;
    }
}

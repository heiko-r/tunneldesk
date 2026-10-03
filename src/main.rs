#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod app_service;
mod capture;
mod cloudflare;
mod cloudflared;
mod config;
mod core;
mod instance;
#[cfg(feature = "mcp")]
mod mcp;
#[cfg(feature = "mcp")]
mod mcp_bridge;
mod proxy;
mod security;
mod storage;
mod sync;
mod tunnel;
mod web_server;

#[cfg(feature = "gui")]
mod gui;

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use clap::{Parser, Subcommand};
use tracing::info;

use crate::config::Config;
use crate::core::{Core, CoreOptions};
use crate::instance::{CoreInfo, CoreLock, InstancePaths, LockError};

#[derive(Parser, Clone)]
#[command(name = "tunneldesk")]
#[command(about = "Local proxy for Cloudflare Tunnels, with request inspection")]
struct Args {
    /// Path to configuration file
    #[arg(short, long, global = true)]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Clone)]
enum Command {
    /// Run the core in the foreground until interrupted
    Serve,
    /// Open the web UI in the system browser, starting the core if needed
    Open,
    /// Stop the core for the config file (removes the Cloudflare routes)
    Stop,
    /// List running cores
    Status,
    /// Run as an MCP server on stdio, bridged to the core's HTTP endpoint
    /// (requires the `mcp` Cargo feature)
    Mcp,
    /// Print MCP client configuration snippets for this config file
    McpConfig,
    /// Run the core process (started automatically by the other commands)
    #[command(hide = true)]
    Core {
        /// Log to the runtime directory and exit when unused
        #[arg(long)]
        detached: bool,
    },
}

/// What this invocation does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Window,
    Serve,
    Core { detached: bool },
    McpBridge,
    Open,
    Stop,
    Status,
    McpConfig,
}

impl Args {
    /// Resolves the config path, falling back to a platform-appropriate default.
    fn resolved_config(&self) -> PathBuf {
        if let Some(path) = &self.config {
            return path.clone();
        }
        default_config_path()
    }

    fn mode(&self) -> Mode {
        match &self.command {
            Some(Command::Serve) => Mode::Serve,
            Some(Command::Open) => Mode::Open,
            Some(Command::Stop) => Mode::Stop,
            Some(Command::Status) => Mode::Status,
            Some(Command::Mcp) => Mode::McpBridge,
            Some(Command::McpConfig) => Mode::McpConfig,
            Some(Command::Core { detached }) => Mode::Core {
                detached: *detached,
            },
            None if !cfg!(feature = "gui") => Mode::Serve,
            None => Mode::Window,
        }
    }
}

/// Returns the default config file path.
///
/// - macOS `.app` bundle: `~/Library/Application Support/TunnelDesk/config.toml`,
///   else `config.toml` in the working directory
/// - Windows: `%APPDATA%\TunnelDesk\config.toml`
/// - Everywhere else: `~/.config/TunnelDesk/config.toml`
fn default_config_path() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        if let Ok(exe) = std::env::current_exe()
            && exe.to_string_lossy().contains(".app/Contents/MacOS/")
            && let Some(home) = std::env::var_os("HOME")
        {
            return PathBuf::from(home).join("Library/Application Support/TunnelDesk/config.toml");
        }
    }
    #[cfg(target_os = "windows")]
    {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            return PathBuf::from(appdata)
                .join("TunnelDesk")
                .join("config.toml");
        }
    }
    #[cfg(unix)]
    {
        // Linux and other Unix-like systems
        if let Some(home_dir) = home::home_dir() {
            return home_dir.join(".config/TunnelDesk/config.toml");
        }
    }
    PathBuf::from("config.toml")
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let mode = args.mode();

    #[cfg(not(feature = "mcp"))]
    if matches!(mode, Mode::McpBridge | Mode::McpConfig) {
        anyhow::bail!(
            "This binary was not compiled with the 'mcp' feature. \
             Rebuild with `--features mcp`."
        );
    }

    let canonical =
        instance::canonical_config_path(&args.resolved_config()).context("invalid config path")?;
    init_tracing(mode, &canonical)?;

    #[cfg(feature = "gui")]
    if mode == Mode::Window {
        let runtime = tokio::runtime::Runtime::new()?;
        let (info, token) = runtime.block_on(attach(&canonical))?;
        drop(runtime);
        gui::launch(&info, &token); // diverges: tao event loop runs forever
    }

    let runtime = tokio::runtime::Runtime::new()?;
    let result = runtime.block_on(run(mode, &canonical));
    if let Err(e) = &result
        && let Some(LockError::AlreadyLocked) = e.downcast_ref::<LockError>()
    {
        eprintln!("{e:#}");
        std::process::exit(instance::EXIT_ALREADY_RUNNING);
    }
    result
}

/// Logs go to stderr for the MCP bridge (stdout carries the protocol), to a
/// file for detached cores, and to stdout otherwise.
fn init_tracing(mode: Mode, canonical: &Path) -> anyhow::Result<()> {
    let builder = tracing_subscriber::fmt().with_max_level(tracing::Level::INFO);
    match mode {
        Mode::McpBridge => builder.with_writer(std::io::stderr).init(),
        Mode::Core { detached: true } => {
            let paths = InstancePaths::for_hash(&instance::config_hash(canonical));
            if let Some(dir) = paths.log.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let file = std::fs::File::create(&paths.log)
                .with_context(|| format!("failed to create {}", paths.log.display()))?;
            builder
                .with_ansi(false)
                .with_writer(std::sync::Mutex::new(file))
                .init();
        }
        _ => builder.init(),
    }
    Ok(())
}

async fn run(mode: Mode, canonical: &Path) -> anyhow::Result<()> {
    match mode {
        Mode::Window => unreachable!("handled before the runtime starts"),
        Mode::Serve => run_core(canonical, false).await,
        Mode::Core { detached } => run_core(canonical, detached).await,
        #[cfg(feature = "mcp")]
        Mode::McpBridge => mcp_bridge::run(canonical).await,
        #[cfg(feature = "mcp")]
        Mode::McpConfig => print_mcp_config(canonical),
        #[cfg(not(feature = "mcp"))]
        Mode::McpBridge | Mode::McpConfig => unreachable!("rejected in main"),
        Mode::Open => {
            let (info, token) = attach(canonical).await?;
            let url = format!("{}/?token={token}", info.base_url());
            open::that(&url).with_context(|| format!("failed to open {}", info.base_url()))?;
            println!("Opened {}", info.base_url());
            Ok(())
        }
        Mode::Stop => {
            let Some(info) = instance::find_core(canonical).await else {
                println!("No TunnelDesk core is running for {}", canonical.display());
                return Ok(());
            };
            instance::stop_core(&info, &read_access_token(canonical)?).await?;
            println!("Stopped TunnelDesk core (pid {})", info.pid);
            Ok(())
        }
        Mode::Status => {
            let cores = instance::live_cores().await;
            if cores.is_empty() {
                println!("No TunnelDesk cores are running");
            }
            for core in cores {
                println!(
                    "pid {:<8} {:<24} {}",
                    core.pid,
                    core.base_url(),
                    core.config_path.display()
                );
            }
            Ok(())
        }
    }
}

/// Finds or starts the core and returns it together with its access token.
async fn attach(canonical: &Path) -> anyhow::Result<(CoreInfo, String)> {
    let info = instance::ensure_core(canonical).await?;
    let token = read_access_token(&info.config_path)?;
    Ok((info, token))
}

fn read_access_token(config_path: &Path) -> anyhow::Result<String> {
    Config::from_file(config_path)
        .with_context(|| format!("failed to read {}", config_path.display()))?
        .gui
        .access_token
        .context("the config has no gui.access_token yet; start the core once to generate it")
}

/// Runs a core for `canonical` until a signal or a shutdown request.
async fn run_core(canonical: &Path, detached: bool) -> anyhow::Result<()> {
    let paths = InstancePaths::for_hash(&instance::config_hash(canonical));
    let lock = match CoreLock::acquire(&paths) {
        Ok(lock) => lock,
        Err(LockError::AlreadyLocked) => {
            let running = instance::read_info(&paths)
                .map(|i| format!(" (pid {}, {})", i.pid, i.base_url()))
                .unwrap_or_default();
            return Err(
                anyhow::Error::new(LockError::AlreadyLocked).context(format!(
                    "a TunnelDesk core for {} is already running{running}",
                    canonical.display()
                )),
            );
        }
        Err(e) => return Err(anyhow::Error::new(e).context("failed to lock the core instance")),
    };

    let core = Core::start(
        canonical,
        CoreOptions {
            idle_shutdown: detached,
        },
    )
    .await?;
    instance::write_info(
        &paths,
        &CoreInfo {
            pid: std::process::id(),
            port: core.port(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            config_path: canonical.to_path_buf(),
            config_hash: instance::config_hash(canonical),
        },
    )
    .context("failed to publish the core info file")?;
    info!("Core for {} is ready", core.config_path().display());

    let shutdown = core.shutdown_token();
    tokio::select! {
        _ = core::shutdown_signal() => {}
        _ = shutdown.cancelled() => {}
    }

    instance::remove_info(&paths);
    core.shutdown().await;
    drop(lock);
    Ok(())
}

#[cfg(feature = "mcp")]
fn print_mcp_config(canonical: &Path) -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;
    let config = Config::load_or_create(canonical)?;
    let stdio = serde_json::json!({
        "mcpServers": { "tunneldesk": {
            "command": exe,
            "args": ["mcp", "--config", canonical],
        }}
    });
    println!("# stdio (starts the core on demand):");
    println!("{}", serde_json::to_string_pretty(&stdio)?);

    println!();
    println!("# Streamable HTTP (the core must already be running):");
    if config.gui.port == 0 {
        println!("# [gui] port = 0 picks a new port on every start; use the stdio variant.");
        return Ok(());
    }
    let token = config
        .gui
        .access_token
        .unwrap_or_else(|| "<start the core once to generate gui.access_token>".into());
    let http = serde_json::json!({
        "mcpServers": { "tunneldesk": {
            "url": format!("http://127.0.0.1:{}/mcp", config.gui.port),
            "headers": { "Authorization": format!("Bearer {token}") },
        }}
    });
    println!("{}", serde_json::to_string_pretty(&http)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Args {
        Args::try_parse_from(std::iter::once("tunneldesk").chain(args.iter().copied())).unwrap()
    }

    #[test]
    fn default_mode_depends_on_gui_feature() {
        let expected = if cfg!(feature = "gui") {
            Mode::Window
        } else {
            Mode::Serve
        };
        assert_eq!(parse(&[]).mode(), expected);
    }

    #[test]
    fn subcommands_map_to_modes() {
        assert_eq!(parse(&["serve"]).mode(), Mode::Serve);
        assert_eq!(parse(&["open"]).mode(), Mode::Open);
        assert_eq!(parse(&["stop"]).mode(), Mode::Stop);
        assert_eq!(parse(&["status"]).mode(), Mode::Status);
        assert_eq!(parse(&["mcp"]).mode(), Mode::McpBridge);
        assert_eq!(parse(&["mcp-config"]).mode(), Mode::McpConfig);
        assert_eq!(
            parse(&["core", "--detached"]).mode(),
            Mode::Core { detached: true }
        );
    }

    #[test]
    fn config_flag_is_global() {
        let args = parse(&["core", "--config", "/tmp/x.toml"]);
        assert_eq!(args.resolved_config(), PathBuf::from("/tmp/x.toml"));
        let args = parse(&["--config", "/tmp/y.toml", "stop"]);
        assert_eq!(args.resolved_config(), PathBuf::from("/tmp/y.toml"));
    }
}

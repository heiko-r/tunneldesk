//! One core per config file: discovery, exclusivity, stop and idle exit.

mod common;

use std::time::Duration;

use common::*;

#[tokio::test]
async fn serve_publishes_a_healthy_core_on_an_ephemeral_port() {
    let env = TestEnv::new();
    let config = env.write_config("config.toml", "");
    let mut core = env.spawn_serve(&config);

    let info = env.wait_for_info(&config).await;
    assert_ne!(info.port, 0, "port = 0 must be resolved to the bound port");
    assert_eq!(info.pid, core.id().unwrap());
    assert_eq!(info.version, env!("CARGO_PKG_VERSION"));

    let health: serde_json::Value = http_client()
        .get(format!("{}/api/health", info.base_url()))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(health["config_hash"], info.config_hash.as_str());

    assert_eq!(
        read_token(&config).len(),
        43,
        "a token is generated on first start"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&config).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "the config holds secrets");
    }

    let (status, stdout, _) = env.run(&config, &["stop"]).await;
    assert!(status.success());
    assert!(stdout.contains("Stopped"), "{stdout}");
    assert!(
        wait_for_exit(&mut core, Duration::from_secs(10))
            .await
            .is_some()
    );
    assert!(env.find_info(&config).is_none());
}

#[tokio::test]
async fn second_core_for_the_same_config_is_refused() {
    let env = TestEnv::new();
    let config = env.write_config("config.toml", "");
    let _core = env.spawn_serve(&config);
    let info = env.wait_for_info(&config).await;

    let (status, _, stderr) = env.run(&config, &["core"]).await;
    assert_eq!(status.code(), Some(EXIT_ALREADY_RUNNING), "{stderr}");
    assert!(stderr.contains("already running"), "{stderr}");
    assert!(stderr.contains(&info.pid.to_string()), "{stderr}");

    // The running core is unaffected.
    assert_eq!(env.find_info(&config).unwrap().pid, info.pid);
    env.run(&config, &["stop"]).await;
}

#[tokio::test]
async fn cores_for_separate_configs_run_side_by_side() {
    let env = TestEnv::new();
    let first = env.write_config("first.toml", "");
    let second = env.write_config("second.toml", "");
    let _a = env.spawn_serve(&first);
    let _b = env.spawn_serve(&second);

    let a = env.wait_for_info(&first).await;
    let b = env.wait_for_info(&second).await;
    assert_ne!(a.port, b.port);
    assert_ne!(a.config_hash, b.config_hash);
    assert_ne!(read_token(&first), read_token(&second));

    let (status, stdout, _) = env.run(&first, &["status"]).await;
    assert!(status.success());
    for info in [&a, &b] {
        assert!(stdout.contains(&info.base_url()), "{stdout}");
        assert!(
            stdout.contains(&info.config_path.display().to_string()),
            "{stdout}"
        );
    }

    // Stopping one core leaves the other running.
    env.run(&first, &["stop"]).await;
    assert!(env.wait_for_info_removed(&first).await);
    assert!(env.find_info(&second).is_some());
    env.run(&second, &["stop"]).await;
}

#[tokio::test]
async fn the_token_of_one_core_does_not_open_another() {
    let env = TestEnv::new();
    let first = env.write_config("first.toml", "");
    let second = env.write_config("second.toml", "");
    let _a = env.spawn_serve(&first);
    let _b = env.spawn_serve(&second);
    let b = env.wait_for_info(&second).await;
    env.wait_for_info(&first).await;

    let response = http_client()
        .post(format!("{}/api/shutdown", b.base_url()))
        .bearer_auth(read_token(&first))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
    assert!(env.find_info(&second).is_some());

    env.run(&first, &["stop"]).await;
    env.run(&second, &["stop"]).await;
}

#[tokio::test]
async fn stop_without_a_running_core_is_a_no_op() {
    let env = TestEnv::new();
    let config = env.write_config("config.toml", "");
    let (status, stdout, _) = env.run(&config, &["stop"]).await;
    assert!(status.success());
    assert!(stdout.contains("No TunnelDesk core is running"), "{stdout}");
}

#[tokio::test]
async fn detached_core_exits_when_idle() {
    let env = TestEnv::new();
    let config = env.write_config("config.toml", "");
    let mut core = env
        .command(&config, &["core", "--detached"])
        .spawn()
        .unwrap();

    env.wait_for_info(&config).await;
    let status = wait_for_exit(&mut core, Duration::from_secs(15))
        .await
        .expect("an unused detached core must exit after idle_timeout_secs");
    assert!(status.success());
    assert!(env.find_info(&config).is_none());

    let log = std::fs::read_dir(env.runtime_dir())
        .unwrap()
        .flatten()
        .find(|e| e.path().extension().is_some_and(|x| x == "log"))
        .expect("detached cores log to the runtime dir");
    assert!(
        std::fs::read_to_string(log.path())
            .unwrap()
            .contains("idle")
    );
}

#[tokio::test]
async fn a_restarted_core_reuses_the_generated_token() {
    let env = TestEnv::new();
    let config = env.write_config("config.toml", "");

    let mut core = env.spawn_serve(&config);
    env.wait_for_info(&config).await;
    let token = read_token(&config);
    env.run(&config, &["stop"]).await;
    wait_for_exit(&mut core, Duration::from_secs(10))
        .await
        .unwrap();

    let _core = env.spawn_serve(&config);
    env.wait_for_info(&config).await;
    assert_eq!(read_token(&config), token);
    env.run(&config, &["stop"]).await;
}

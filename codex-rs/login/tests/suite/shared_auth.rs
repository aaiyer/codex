#![cfg(target_os = "linux")]
use anyhow::Context;
use anyhow::Result;
use base64::Engine;
use codex_config::types::AuthCredentialsStoreMode;
use codex_login::AuthKeyringBackendKind;
use codex_login::AuthManager;
use codex_login::auth::AuthConfig;
use codex_login::auth::read_auth_json;
use codex_protocol::config_types::ForcedLoginMethod;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Child;
use std::process::Command;
use std::time::Duration;
use std::time::Instant;

const TEST: &str = "suite::shared_auth::shared_auth_process_contract";

fn payload(account: &str, access: &str, refresh: &str) -> Vec<u8> {
    let claims = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
        json!({"https://api.openai.com/auth": {"chatgpt_account_id": account, "chatgpt_user_id": account}}).to_string());
    json!({"auth_mode":"chatgpt", "tokens": {
        "id_token":format!("e30.{claims}.signature"), "access_token":access,
        "refresh_token":refresh, "account_id":account}, "last_refresh":chrono::Utc::now()})
    .to_string()
    .into_bytes()
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut options = std::fs::OpenOptions::new();
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = options
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    Ok(())
}

fn wait_ready(path: &Path) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !path.exists() {
        anyhow::ensure!(Instant::now() < deadline, "child readiness timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}

fn join_child(mut child: Child) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(status) = child.try_wait()? {
            anyhow::ensure!(status.success(), "auth subprocess failed: {status}");
            return Ok(());
        }
        if Instant::now() >= deadline {
            child.kill()?;
            child.wait()?;
            anyhow::bail!("auth subprocess timed out");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[tokio::test]
async fn shared_auth_process_contract() -> Result<()> {
    if let Ok(action) = std::env::var("CODEX_SHARED_AUTH_TEST_ACTION") {
        let home = tempfile::tempdir()?;
        let root = std::env::var("CODEX_AUTH_HOME")?;
        let config = AuthConfig {
            codex_home: home.path().to_path_buf(),
            auth_credentials_store_mode: AuthCredentialsStoreMode::Auto,
            keyring_backend_kind: AuthKeyringBackendKind::default(),
            forced_login_method: None,
            chatgpt_base_url: None,
            forced_chatgpt_workspace_id: None,
            managed_auth_policy: Default::default(),
            auth_route_config: codex_login::test_support::transport_default_auth_route_config(),
        };
        let manager = AuthManager::shared_from_auth_config(config.clone(), true).await?;
        if action == "reload" {
            let first = manager.auth().await.context("shared auth should load")?;
            assert_eq!(first.get_account_id().as_deref(), Some("account-a"));
            let receiver = manager.auth_change_state_receiver();
            let owner = receiver.borrow().owner_generation;
            config
                .import_auth_json(payload("account-a", "access-new", "refresh-new").as_slice())
                .await?;
            assert_eq!(manager.auth().await.unwrap().get_token()?, "access-new");
            assert_eq!(receiver.borrow().owner_generation, owner);
            config
                .import_auth_json(payload("account-b", "access-b", "refresh-b").as_slice())
                .await?;
            assert_eq!(
                manager.auth().await.unwrap().get_account_id().as_deref(),
                Some("account-b")
            );
            assert!(receiver.borrow().owner_generation > owner);
            let mut restricted = config.clone();
            restricted.forced_login_method = Some(ForcedLoginMethod::Api);
            assert!(
                restricted
                    .import_auth_json(payload("account-a", "access-a", "refresh-a").as_slice())
                    .await
                    .is_err()
            );
            assert_eq!(
                manager.auth().await.unwrap().get_account_id().as_deref(),
                Some("account-b")
            );
            let isolated = AuthManager::from_auth_for_testing(
                codex_login::CodexAuth::from_api_key("isolated"),
            );
            assert_eq!(isolated.auth().await.unwrap().get_token()?, "isolated");
            assert!(isolated.logout().await.is_err());
            std::fs::remove_file(Path::new(&root).join("auth.json"))?;
            assert!(manager.auth().await.is_none());
            write_private(&Path::new(&root).join("auth.json"), b"{invalid}")?;
            assert!(manager.auth().await.is_none());
            assert!(manager.auth_cached().is_none());
            assert!(!home.path().join("auth.json").exists());
            return Ok(());
        }
        let control = std::env::var("CODEX_SHARED_AUTH_TEST_CONTROL")?;
        std::fs::write(Path::new(&control).join(format!("{action}.ready")), b"")?;
        wait_ready(&Path::new(&control).join(format!("{action}.go")))?;
        if action == "first" {
            manager.refresh_token().await?;
        } else {
            manager.refresh_token_from_authority().await?;
        }
        assert_eq!(
            manager.auth().await.unwrap().get_token()?,
            "access-refreshed"
        );
        return Ok(());
    }
    let root = tempfile::tempdir()?;
    let control = tempfile::tempdir()?;
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700))?;
    let auth_path = root.path().join("auth.json");
    write_private(&auth_path, &payload("account-a", "access-a", "refresh-a"))?;
    let server =
        tiny_http::Server::http("127.0.0.1:0").map_err(|error| anyhow::anyhow!("{error}"))?;
    let endpoint = format!("http://{}/oauth/token", server.server_addr());
    let spawn = |action: &str| -> Result<Child> {
        Ok(Command::new(std::env::current_exe()?)
            .arg("--exact")
            .arg(TEST)
            .arg("--nocapture")
            .env("CODEX_SHARED_AUTH_TEST_ACTION", action)
            .env("CODEX_SHARED_AUTH_TEST_CONTROL", control.path())
            .env("CODEX_AUTH_HOME", root.path())
            .env("CODEX_API_KEY", "poison-env-key")
            .env("CODEX_ACCESS_TOKEN", "poison-env-token")
            .env(codex_login::REFRESH_TOKEN_URL_OVERRIDE_ENV_VAR, &endpoint)
            .spawn()?)
    };
    join_child(spawn("reload")?)?;
    write_private(&auth_path, &payload("account-a", "access-a", "refresh-a"))?;
    let first = spawn("first")?;
    wait_ready(&control.path().join("first.ready"))?;
    std::fs::write(control.path().join("first.go"), b"")?;
    let mut request = server
        .recv_timeout(Duration::from_secs(20))?
        .context("first refresh request")?;
    let mut body = String::new();
    request.as_reader().read_to_string(&mut body)?;
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&body)?["refresh_token"],
        "refresh-a"
    );
    // The second process has cached A while the first process holds the writer lease at OAuth.
    let second = spawn("second")?;
    wait_ready(&control.path().join("second.ready"))?;
    let lock = std::fs::File::open(root.path().join("auth.lock"))?;
    assert!(matches!(
        lock.try_lock(),
        Err(std::fs::TryLockError::WouldBlock)
    ));
    std::fs::write(control.path().join("second.go"), b"")?;
    request.respond(
        tiny_http::Response::from_string(
            r#"{"access_token":"access-refreshed","refresh_token":"refresh-refreshed"}"#,
        )
        .with_header(tiny_http::Header::from_bytes("Content-Type", "application/json").unwrap()),
    )?;
    join_child(first)?;
    join_child(second)?;
    assert!(
        server.try_recv()?.is_none(),
        "second process must reread instead of refreshing the old token"
    );
    let stored = read_auth_json(std::fs::File::open(auth_path)?)?;
    assert_eq!(stored.tokens.unwrap().refresh_token, "refresh-refreshed");
    Ok(())
}

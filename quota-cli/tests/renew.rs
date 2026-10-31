//! Covers renewing Claude and Codex tokens against a local stand-in for the
//! token endpoints, and what ends up in the stored login afterwards.

use quota_core::claude::local::ClaudeLogin;
use quota_core::claude::refresh::{self as claude_refresh, RenewOutcome as ClaudeOutcome};
use quota_core::codex::refresh::{self as codex_refresh, RenewOutcome as CodexOutcome};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

const NOW_MS: i64 = 1_800_000_000_000;

fn now_ms() -> i64 {
    NOW_MS
}

fn now_utc() -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::from_timestamp_millis(NOW_MS).expect("time")
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("quota-renew-{}-{}", std::process::id(), name));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

/// Answer one HTTP request with `status` and `body`, running `before_reply`
/// first. Returns the URL to call and a receiver for the request body.
fn serve_once(
    status: u16,
    body: Value,
    before_reply: impl FnOnce() + Send + 'static,
) -> (String, mpsc::Receiver<Value>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let url = format!("http://{}/token", listener.local_addr().expect("addr"));
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let (stream, _) = listener.accept().expect("accept");
        let mut reader = BufReader::new(stream.try_clone().expect("clone"));
        let mut length = 0usize;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).expect("read line");
            if line == "\r\n" || line.is_empty() {
                break;
            }
            if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                length = value.trim().parse().expect("length");
            }
        }
        let mut request = vec![0u8; length];
        reader.read_exact(&mut request).expect("read body");
        sender
            .send(serde_json::from_slice(&request).expect("json body"))
            .expect("send");
        before_reply();
        let text = body.to_string();
        let mut stream = stream;
        write!(
            stream,
            "HTTP/1.1 {} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            status,
            text.len(),
            text
        )
        .expect("write");
    });
    (url, receiver)
}

fn read_json(path: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).expect("read")).expect("json")
}

#[cfg(unix)]
fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .expect("metadata")
        .permissions()
        .mode()
        & 0o777
}

fn claude_login(name: &str, expires_at: i64) -> (ClaudeLogin, PathBuf) {
    let dir = temp_dir(name);
    let credentials = dir.join(".credentials.json");
    std::fs::write(
        &credentials,
        json!({
            "claudeAiOauth": {
                "accessToken": "old-access",
                "refreshToken": "old-refresh",
                "expiresAt": expires_at,
                "scopes": ["user:inference", "user:profile"],
                "subscriptionType": "max",
                "rateLimitTier": "default_claude_max_20x"
            },
            "mcpOAuth": {"server": {"accessToken": "keep-me"}}
        })
        .to_string(),
    )
    .expect("write credentials");
    (
        ClaudeLogin::for_config_dir(&dir.to_string_lossy()),
        credentials,
    )
}

#[tokio::test]
async fn an_expired_claude_token_is_renewed_and_saved_like_claude_code_saves_it() {
    let (login, credentials) = claude_login("claude-renew", NOW_MS - 1);
    let (url, requests) = serve_once(
        200,
        json!({
            "access_token": "new-access",
            "refresh_token": "new-refresh",
            "expires_in": 28_800,
            "refresh_token_expires_in": 2_592_000,
            "scope": "user:inference user:profile",
            "token_type": "Bearer"
        }),
        || {},
    );

    let outcome = claude_refresh::renew_with(&login, None, &url, now_ms)
        .await
        .expect("renew");
    assert_eq!(outcome, ClaudeOutcome::Renewed);

    let sent = requests.recv().expect("request");
    assert_eq!(sent["grant_type"], "refresh_token");
    assert_eq!(sent["refresh_token"], "old-refresh");
    assert_eq!(sent["client_id"], claude_refresh::CLIENT_ID);
    assert_eq!(sent["scope"], "user:inference user:profile");

    let saved = read_json(&credentials);
    let oauth = &saved["claudeAiOauth"];
    assert_eq!(oauth["accessToken"], "new-access");
    assert_eq!(oauth["refreshToken"], "new-refresh");
    assert_eq!(oauth["expiresAt"], NOW_MS + 28_800_000);
    assert_eq!(oauth["refreshTokenExpiresAt"], NOW_MS + 2_592_000_000);
    // What the response does not replace stays as Claude Code wrote it.
    assert_eq!(oauth["subscriptionType"], "max");
    assert_eq!(oauth["rateLimitTier"], "default_claude_max_20x");
    assert_eq!(saved["mcpOAuth"]["server"]["accessToken"], "keep-me");
    #[cfg(unix)]
    assert_eq!(mode(&credentials), 0o600);

    // Both of Claude Code's locks are gone again.
    assert!(!login.config_dir().join(".oauth_refresh.lock").exists());
    let mut legacy = std::fs::canonicalize(login.config_dir())
        .expect("canonical")
        .into_os_string();
    legacy.push(".lock");
    assert!(!PathBuf::from(legacy).exists());
}

#[tokio::test]
async fn a_claude_token_that_is_still_good_is_left_alone() {
    let (login, credentials) = claude_login("claude-fresh", NOW_MS + 60 * 60 * 1000);
    let before = std::fs::read_to_string(&credentials).expect("read");

    // Nothing listens here: any request would fail the test.
    let outcome = claude_refresh::renew_with(&login, None, "http://127.0.0.1:9/token", now_ms)
        .await
        .expect("renew");
    assert_eq!(outcome, ClaudeOutcome::Fresh);

    // A rejected token that is no longer the stored one means someone else
    // already renewed, so there is nothing to do either.
    let outcome = claude_refresh::renew_with(
        &login,
        Some("older-access"),
        "http://127.0.0.1:9/token",
        now_ms,
    )
    .await
    .expect("renew");
    assert_eq!(outcome, ClaudeOutcome::Fresh);
    assert_eq!(std::fs::read_to_string(&credentials).expect("read"), before);
}

#[tokio::test]
async fn a_claude_token_the_api_rejected_is_renewed_even_before_it_expires() {
    let (login, credentials) = claude_login("claude-rejected", NOW_MS + 60 * 60 * 1000);
    let (url, _requests) = serve_once(
        200,
        json!({"access_token": "new-access", "expires_in": 3600}),
        || {},
    );

    let outcome = claude_refresh::renew_with(&login, Some("old-access"), &url, now_ms)
        .await
        .expect("renew");
    assert_eq!(outcome, ClaudeOutcome::Renewed);
    // No new refresh token in the answer keeps the one that was sent.
    assert_eq!(
        read_json(&credentials)["claudeAiOauth"]["refreshToken"],
        "old-refresh"
    );
}

#[tokio::test]
async fn a_refused_claude_refresh_token_asks_for_a_new_sign_in_and_changes_nothing() {
    let (login, credentials) = claude_login("claude-refused", NOW_MS - 1);
    let before = std::fs::read_to_string(&credentials).expect("read");
    let (url, _requests) = serve_once(
        400,
        json!({"error": "invalid_grant", "error_description": "Refresh token not found or invalid"}),
        || {},
    );

    let error = claude_refresh::renew_with(&login, None, &url, now_ms)
        .await
        .expect_err("refused");
    assert!(error.needs_sign_in());
    assert_eq!(std::fs::read_to_string(&credentials).expect("read"), before);
}

#[tokio::test]
async fn a_claude_login_renewed_by_someone_else_meanwhile_is_not_overwritten() {
    let (login, credentials) = claude_login("claude-race", NOW_MS - 1);
    let path = credentials.clone();
    let (url, _requests) = serve_once(
        200,
        json!({"access_token": "ours", "refresh_token": "ours-refresh", "expires_in": 3600}),
        move || {
            // Another Claude Code process saves its own renewal first.
            let mut document = read_json(&path);
            document["claudeAiOauth"]["accessToken"] = json!("theirs");
            document["claudeAiOauth"]["refreshToken"] = json!("theirs-refresh");
            std::fs::write(&path, document.to_string()).expect("write");
        },
    );

    let outcome = claude_refresh::renew_with(&login, None, &url, now_ms)
        .await
        .expect("renew");
    assert_eq!(outcome, ClaudeOutcome::RenewedElsewhere);
    assert_eq!(
        read_json(&credentials)["claudeAiOauth"]["accessToken"],
        "theirs"
    );
}

// Windows cannot open a directory to set its time this way.
#[cfg(unix)]
#[tokio::test]
async fn a_claude_lock_left_behind_by_a_dead_process_is_taken_over() {
    let (login, _credentials) = claude_login("claude-stale-lock", NOW_MS - 1);
    let lock = login.config_dir().join(".oauth_refresh.lock");
    std::fs::create_dir(&lock).expect("lock");
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(120);
    std::fs::File::open(&lock)
        .expect("open lock")
        .set_modified(old)
        .expect("age lock");
    let (url, _requests) = serve_once(
        200,
        json!({"access_token": "new-access", "expires_in": 3600}),
        || {},
    );

    let outcome = claude_refresh::renew_with(&login, None, &url, now_ms)
        .await
        .expect("renew");
    assert_eq!(outcome, ClaudeOutcome::Renewed);
    assert!(!lock.exists());
}

fn base64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n =
            chunk.iter().fold(0u32, |n, byte| (n << 8) | *byte as u32) << (8 * (3 - chunk.len()));
        for i in 0..=chunk.len() {
            out.push(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize] as char);
        }
    }
    out
}

fn jwt(claims: Value) -> String {
    format!(
        "{}.{}.sig",
        base64url(br#"{"alg":"none"}"#),
        base64url(claims.to_string().as_bytes())
    )
}

fn codex_home(name: &str, access_expires_at: i64) -> PathBuf {
    let home = temp_dir(name);
    std::fs::write(
        home.join("auth.json"),
        serde_json::to_string_pretty(&json!({
            "auth_mode": "chatgpt",
            "OPENAI_API_KEY": null,
            "tokens": {
                "id_token": jwt(json!({"email": "me@example.com"})),
                "access_token": jwt(json!({"exp": access_expires_at})),
                "refresh_token": "old-refresh",
                "account_id": "acct-1",
                "login_owner_id": "owner-1",
                "oauth_provenance": {"issuer": "https://auth.openai.com", "client_id": "app_EMoamEEZ73f0CkXaXp7hrann"}
            },
            "last_refresh": "2026-09-11T21:15:52.609002Z"
        }))
        .expect("json"),
    )
    .expect("write auth.json");
    home
}

#[tokio::test]
async fn an_expired_codex_token_is_renewed_and_saved_like_the_codex_cli_saves_it() {
    let home = codex_home("codex-renew", NOW_MS / 1000 - 60);
    let new_access = jwt(json!({"exp": NOW_MS / 1000 + 864_000}));
    let (url, requests) = serve_once(
        200,
        json!({
            "id_token": jwt(json!({"email": "me@example.com"})),
            "access_token": new_access,
            "refresh_token": "new-refresh"
        }),
        || {},
    );

    let outcome = codex_refresh::renew_with(&home, None, Some(&url), now_utc)
        .await
        .expect("renew");
    assert_eq!(outcome, CodexOutcome::Renewed);

    let sent = requests.recv().expect("request");
    assert_eq!(sent["grant_type"], "refresh_token");
    assert_eq!(sent["refresh_token"], "old-refresh");
    assert_eq!(sent["client_id"], codex_refresh::CLIENT_ID);

    let saved = read_json(&home.join("auth.json"));
    assert_eq!(saved["tokens"]["access_token"], new_access.as_str());
    assert_eq!(saved["tokens"]["refresh_token"], "new-refresh");
    // The CLI refuses a file whose account changed, so these must survive.
    assert_eq!(saved["tokens"]["account_id"], "acct-1");
    assert_eq!(saved["tokens"]["login_owner_id"], "owner-1");
    assert_eq!(
        saved["tokens"]["oauth_provenance"]["issuer"],
        "https://auth.openai.com"
    );
    assert_eq!(saved["auth_mode"], "chatgpt");
    assert_eq!(saved["OPENAI_API_KEY"], Value::Null);
    assert_eq!(saved["last_refresh"], "2027-01-15T08:00:00.000000Z");
    #[cfg(unix)]
    assert_eq!(mode(&home.join("auth.json")), 0o600);

    // The renewed login reads back as before, with the new token.
    let credentials =
        quota_core::codex::local::read_local_credentials_at(&home.join("auth.json")).expect("read");
    assert_eq!(credentials.access_token, new_access);
    assert_eq!(
        credentials.identity().as_deref(),
        Some("acct-1:me@example.com")
    );
}

#[tokio::test]
async fn a_codex_token_that_is_still_good_is_left_alone() {
    let home = codex_home("codex-fresh", NOW_MS / 1000 + 3600);
    let before = std::fs::read_to_string(home.join("auth.json")).expect("read");
    let outcome = codex_refresh::renew_with(&home, None, Some("http://127.0.0.1:9/token"), now_utc)
        .await
        .expect("renew");
    assert_eq!(outcome, CodexOutcome::Fresh);
    assert_eq!(
        std::fs::read_to_string(home.join("auth.json")).expect("read"),
        before
    );
}

#[tokio::test]
async fn a_refused_codex_refresh_token_asks_for_a_new_sign_in() {
    let home = codex_home("codex-refused", NOW_MS / 1000 - 60);
    let before = std::fs::read_to_string(home.join("auth.json")).expect("read");
    let (url, _requests) = serve_once(
        401,
        json!({"error": {"code": "refresh_token_expired"}}),
        || {},
    );

    let error = codex_refresh::renew_with(&home, None, Some(&url), now_utc)
        .await
        .expect_err("refused");
    assert!(error.needs_sign_in());
    assert_eq!(
        std::fs::read_to_string(home.join("auth.json")).expect("read"),
        before
    );
}

#[tokio::test]
async fn a_codex_refresh_token_spent_by_the_cli_meanwhile_adopts_its_result() {
    let home = codex_home("codex-race", NOW_MS / 1000 - 60);
    let path = home.join("auth.json");
    let (url, _requests) = serve_once(400, json!({"error": "refresh_token_reused"}), move || {
        // The Codex CLI renewed with the same token a moment earlier.
        let mut document = read_json(&path);
        document["tokens"]["refresh_token"] = json!("cli-refresh");
        std::fs::write(&path, document.to_string()).expect("write");
    });

    let outcome = codex_refresh::renew_with(&home, None, Some(&url), now_utc)
        .await
        .expect("adopt");
    assert_eq!(outcome, CodexOutcome::RenewedElsewhere);
    assert_eq!(
        read_json(&home.join("auth.json"))["tokens"]["refresh_token"],
        "cli-refresh"
    );
}

#[tokio::test]
async fn an_api_key_login_has_nothing_to_renew() {
    let home = temp_dir("codex-api-key");
    std::fs::write(home.join("auth.json"), r#"{"OPENAI_API_KEY":"sk-test"}"#).expect("write");
    let outcome = codex_refresh::renew_with(&home, None, Some("http://127.0.0.1:9/token"), now_utc)
        .await
        .expect("renew");
    assert_eq!(outcome, CodexOutcome::Fresh);
}

/// Deletes the Keychain item a test made, even when the test fails.
#[cfg(target_os = "macos")]
struct KeychainItem {
    service: String,
}

#[cfg(target_os = "macos")]
impl Drop for KeychainItem {
    fn drop(&mut self) {
        for _ in 0..4 {
            let _ = std::process::Command::new("/usr/bin/security")
                .args(["delete-generic-password", "-s", &self.service])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        }
    }
}

/// The whole renewal against a real Keychain item, which this test makes and
/// deletes again. It needs an unlocked login Keychain, so it only runs when
/// asked for: `cargo test --test renew -- --ignored`.
#[cfg(target_os = "macos")]
#[tokio::test]
#[ignore]
async fn renews_a_login_kept_in_the_macos_keychain() {
    let dir = temp_dir("claude-keychain");
    let login = ClaudeLogin::for_config_dir(&dir.to_string_lossy());
    let service = login.keychain_service();
    let account = std::env::var("USER").expect("USER");
    let _item = KeychainItem {
        service: service.clone(),
    };
    let stored = json!({
        "claudeAiOauth": {
            "accessToken": "fake-old-access",
            "refreshToken": "fake-old-refresh",
            "expiresAt": NOW_MS - 1,
            "scopes": ["user:inference", "user:profile"],
            "subscriptionType": "max"
        }
    });
    let created = std::process::Command::new("/usr/bin/security")
        .args(["add-generic-password", "-a", &account, "-s", &service, "-w"])
        .arg(stored.to_string())
        .status()
        .expect("security");
    assert!(created.success());

    let (url, requests) = serve_once(
        200,
        json!({"access_token": "fake-new-access", "refresh_token": "fake-new-refresh", "expires_in": 3600}),
        || {},
    );
    let outcome = claude_refresh::renew_with(&login, None, &url, now_ms)
        .await
        .expect("renew");
    assert_eq!(outcome, ClaudeOutcome::Renewed);
    assert_eq!(
        requests.recv().expect("request")["refresh_token"],
        "fake-old-refresh"
    );

    // The item was updated in place: one item, new tokens, the rest kept.
    let read = std::process::Command::new("/usr/bin/security")
        .args([
            "find-generic-password",
            "-a",
            &account,
            "-s",
            &service,
            "-w",
        ])
        .output()
        .expect("security");
    assert!(read.status.success());
    let saved: Value = serde_json::from_slice(&read.stdout).expect("json");
    assert_eq!(saved["claudeAiOauth"]["accessToken"], "fake-new-access");
    assert_eq!(saved["claudeAiOauth"]["refreshToken"], "fake-new-refresh");
    assert_eq!(saved["claudeAiOauth"]["subscriptionType"], "max");
    assert!(!dir.join(".credentials.json").exists());

    let deleted = std::process::Command::new("/usr/bin/security")
        .args(["delete-generic-password", "-s", &service])
        .stdout(std::process::Stdio::null())
        .status()
        .expect("security");
    assert!(deleted.success());
    let again = std::process::Command::new("/usr/bin/security")
        .args(["find-generic-password", "-s", &service])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("security");
    assert_eq!(again.code(), Some(44), "a second item was left behind");
}

#[cfg(unix)]
#[tokio::test]
async fn a_codex_login_that_could_not_be_saved_is_not_renewed_at_all() {
    use std::os::unix::fs::PermissionsExt;

    let home = codex_home("codex-read-only", NOW_MS / 1000 - 60);
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o500)).expect("chmod");
    // Nothing listens here: the refresh token must not be sent at all.
    let result =
        codex_refresh::renew_with(&home, None, Some("http://127.0.0.1:9/token"), now_utc).await;
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).expect("chmod");

    let error = result.expect_err("cannot save");
    assert!(!error.needs_sign_in(), "{}", error);
    assert!(error.to_string().contains("auth.lock"), "{}", error);
}

#[tokio::test]
async fn two_renewals_of_one_codex_login_spend_its_refresh_token_once() {
    let home = codex_home("codex-twice", NOW_MS / 1000 - 60);
    // This stand-in answers a single request, as the token endpoint would
    // answer only the first use of a refresh token.
    let (url, _requests) = serve_once(
        200,
        json!({"access_token": jwt(json!({"exp": NOW_MS / 1000 + 3600})), "refresh_token": "new-refresh"}),
        || {},
    );

    let (first, second) = tokio::join!(
        codex_refresh::renew_with(&home, None, Some(&url), now_utc),
        codex_refresh::renew_with(&home, None, Some(&url), now_utc),
    );
    let mut outcomes = vec![first.expect("first"), second.expect("second")];
    outcomes.sort_by_key(|outcome| format!("{:?}", outcome));
    assert_eq!(
        outcomes,
        vec![CodexOutcome::Renewed, CodexOutcome::RenewedElsewhere]
    );
    assert_eq!(
        read_json(&home.join("auth.json"))["tokens"]["refresh_token"],
        "new-refresh"
    );
}

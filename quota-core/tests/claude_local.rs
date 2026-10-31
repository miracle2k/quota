use quota_core::claude::local::{
    keychain_service_for_config_dir, parse_account_identity, parse_credentials_json,
    read_local_credentials_at, ClaudeLogin, LocalCredentialError, KEYCHAIN_SERVICE,
};
use std::fs;

fn temp_file(name: &str, body: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "quota-claude-local-{}-{}",
        std::process::id(),
        name
    ));
    fs::create_dir_all(&dir).expect("create temp dir");
    let path = dir.join(".credentials.json");
    fs::write(&path, body).expect("write credentials");
    path
}

#[test]
fn reads_a_valid_credentials_file() {
    let far_future = 4_102_444_800_000i64;
    let path = temp_file(
        "valid",
        &format!(
            r#"{{"claudeAiOauth":{{"accessToken":"tok","refreshToken":"r","expiresAt":{},"scopes":["user:profile"],"subscriptionType":"pro"}},"organizationUuid":"org-1"}}"#,
            far_future
        ),
    );

    let creds = read_local_credentials_at(&path).expect("parse");
    assert_eq!(creds.access_token, "tok");
    assert_eq!(creds.subscription_type.as_deref(), Some("pro"));
    assert_eq!(creds.organization_uuid.as_deref(), Some("org-1"));
    assert!(!creds.is_expired_at_ms(far_future - 1));
}

#[test]
fn reports_expiry_without_refreshing() {
    let past = 1_000_000_000_000i64;
    let path = temp_file(
        "expired",
        &format!(
            r#"{{"claudeAiOauth":{{"accessToken":"tok","refreshToken":"r","expiresAt":{}}}}}"#,
            past
        ),
    );

    let creds = read_local_credentials_at(&path).expect("parse");
    assert!(creds.is_expired_at_ms(past + 1));
}

#[test]
fn missing_file_is_a_distinct_error() {
    let path = std::env::temp_dir().join("quota-claude-local-does-not-exist/.credentials.json");
    match read_local_credentials_at(&path) {
        Err(LocalCredentialError::NotFound) => {}
        other => panic!("expected NotFound, got {:?}", other),
    }
}

#[test]
fn a_file_without_the_oauth_block_is_malformed() {
    let path = temp_file("empty", r#"{"organizationUuid":"org-1"}"#);
    match read_local_credentials_at(&path) {
        Err(LocalCredentialError::Malformed(_)) => {}
        other => panic!("expected Malformed, got {:?}", other),
    }
}

#[test]
fn credentials_struct_debug_redacts_access_token() {
    let path = temp_file("leak", r#"{"claudeAiOauth":{"accessToken":"super-secret"}}"#);
    let rendered = format!("{:?}", read_local_credentials_at(&path));
    assert!(!rendered.contains("super-secret"));
}

#[test]
fn malformed_error_never_leaks_token_in_debug_or_display() {
    // Force a parse error where the entire file content is a token-like string
    // This will cause serde to emit an error message that quotes the token
    let path = temp_file("token-leak-test", r#""super-secret-token""#);
    let result = read_local_credentials_at(&path);

    match result {
        Err(err) => {
            let debug_output = format!("{:?}", err);
            let display_output = format!("{}", err);

            assert!(
                !debug_output.contains("super-secret-token"),
                "Debug output leaked token: {}",
                debug_output
            );
            assert!(
                !display_output.contains("super-secret-token"),
                "Display output leaked token: {}",
                display_output
            );
        }
        Ok(_) => panic!("Expected Malformed error but got Ok"),
    }
}

#[test]
fn parses_the_payload_the_macos_keychain_returns() {
    // `security find-generic-password -w` prints the same JSON the file holds,
    // followed by a newline that the reader trims before parsing.
    let raw = r#"{"claudeAiOauth":{"accessToken":"tok","expiresAt":4102444800000,"scopes":["user:inference"],"subscriptionType":"max"}}"#;

    let creds = parse_credentials_json(raw).expect("parse");
    assert_eq!(creds.access_token, "tok");
    assert_eq!(creds.subscription_type.as_deref(), Some("max"));
    assert_eq!(creds.scopes, vec!["user:inference".to_string()]);
    assert!(creds.organization_uuid.is_none());
}

#[test]
fn the_keychain_service_matches_what_claude_code_writes() {
    // A typo here means the macOS fallback silently finds nothing.
    assert_eq!(KEYCHAIN_SERVICE, "Claude Code-credentials");
}

#[test]
fn a_config_dir_login_uses_the_keychain_item_claude_code_names_after_it() {
    // Claude Code appends the first eight hex digits of the SHA-256 of the
    // CLAUDE_CONFIG_DIR value. The expected value was computed independently.
    assert_eq!(
        keychain_service_for_config_dir("/Users/test/.claude-work"),
        "Claude Code-credentials-03abf0ee"
    );

    let login = ClaudeLogin::for_config_dir("/Users/test/.claude-work");
    assert_eq!(login.keychain_service(), "Claude Code-credentials-03abf0ee");
    assert_eq!(
        login.credentials_path(),
        std::path::PathBuf::from("/Users/test/.claude-work/.credentials.json")
    );
    assert_eq!(
        login.account_file_path(),
        Some(std::path::PathBuf::from(
            "/Users/test/.claude-work/.claude.json"
        ))
    );
}

#[test]
fn the_default_login_uses_the_plain_keychain_item_and_the_home_account_file() {
    let login = ClaudeLogin::default_dir().expect("home directory");
    let home = dirs::home_dir().expect("home directory");

    assert_eq!(login.keychain_service(), KEYCHAIN_SERVICE);
    assert_eq!(
        login.credentials_path(),
        home.join(".claude").join(".credentials.json")
    );
    assert_eq!(login.account_file_path(), Some(home.join(".claude.json")));
}

#[test]
fn a_config_dir_login_reads_the_credentials_file_inside_it() {
    let path = temp_file(
        "config-dir",
        r#"{"claudeAiOauth":{"accessToken":"secret-token","subscriptionType":"max"}}"#,
    );
    let dir = path
        .parent()
        .expect("parent")
        .to_string_lossy()
        .into_owned();

    let credentials = ClaudeLogin::for_config_dir(&dir)
        .read_credentials()
        .expect("credentials");

    assert_eq!(credentials.access_token, "secret-token");
    assert_eq!(credentials.subscription_type.as_deref(), Some("max"));
}

#[test]
fn reads_the_signed_in_account_from_the_claude_json_file() {
    let raw = r#"{
        "numStartups": 3,
        "projects": { "/tmp/x": { "history": [] } },
        "oauthAccount": {
            "accountUuid": "account-1",
            "organizationUuid": "org-1",
            "emailAddress": "someone@example.com"
        }
    }"#;
    assert_eq!(
        parse_account_identity(raw).as_deref(),
        Some("org-1:account-1")
    );

    assert_eq!(parse_account_identity(r#"{"numStartups": 3}"#), None);
    assert_eq!(parse_account_identity("not json"), None);
}

#[test]
fn reads_the_account_and_its_address_from_the_claude_json_file() {
    use quota_core::claude::local::{parse_account_info, AccountInfo};

    let raw = r#"{"oauthAccount": {
        "accountUuid": "account-1",
        "organizationUuid": "org-1",
        "emailAddress": " someone@example.com "
    }}"#;
    assert_eq!(
        parse_account_info(raw),
        Some(AccountInfo {
            account_uuid: Some("account-1".to_string()),
            organization_uuid: Some("org-1".to_string()),
            email: Some("someone@example.com".to_string()),
        })
    );
    assert_eq!(parse_account_info(r#"{"oauthAccount": {}}"#), None);
    assert_eq!(parse_account_info(r#"{"numStartups": 3}"#), None);
}

#[test]
fn finds_the_account_name_in_keychain_item_attributes() {
    use quota_core::claude::local::parse_keychain_account_attribute;

    // What `security find-generic-password -s <service>` prints, minus the
    // secret, which it only prints when asked.
    let attributes = r#"keychain: "/Users/test/Library/Keychains/login.keychain-db"
version: 512
class: "genp"
attributes:
    0x00000007 <blob>="Claude Code-credentials"
    "acct"<blob>="someone"
    "svce"<blob>="Claude Code-credentials"
"#;
    assert_eq!(
        parse_keychain_account_attribute(attributes).as_deref(),
        Some("someone")
    );
    assert_eq!(parse_keychain_account_attribute("attributes:\n"), None);
}

#[test]
fn a_renewal_keeps_what_claude_code_stored_and_replaces_the_tokens() {
    use quota_core::claude::refresh::{apply_token_response, TokenResponse};

    let mut document = serde_json::json!({
        "claudeAiOauth": {
            "accessToken": "old", "refreshToken": "old-refresh", "expiresAt": 1,
            "scopes": ["user:inference"], "subscriptionType": "pro"
        },
        "trustedDeviceToken": "keep"
    });
    let response = TokenResponse {
        access_token: "new".to_string(),
        refresh_token: None,
        expires_in: Some(60),
        refresh_token_expires_in: None,
        scope: Some("user:inference user:profile".to_string()),
    };
    apply_token_response(&mut document, &response, "old-refresh", 1_000).expect("apply");

    let oauth = &document["claudeAiOauth"];
    assert_eq!(oauth["accessToken"], "new");
    assert_eq!(oauth["refreshToken"], "old-refresh");
    assert_eq!(oauth["expiresAt"], 61_000);
    assert_eq!(oauth["scopes"], serde_json::json!(["user:inference", "user:profile"]));
    assert_eq!(oauth["subscriptionType"], "pro");
    assert!(oauth.get("refreshTokenExpiresAt").is_none());
    assert_eq!(document["trustedDeviceToken"], "keep");
}

#[test]
fn only_a_refused_refresh_token_asks_for_a_new_sign_in() {
    use quota_core::claude::refresh::classify_failure;

    assert!(classify_failure(400, r#"{"error":"invalid_grant"}"#).needs_sign_in());
    assert!(classify_failure(401, "").needs_sign_in());
    assert!(!classify_failure(400, r#"{"error":"invalid_request"}"#).needs_sign_in());
    assert!(!classify_failure(500, "oops").needs_sign_in());
    assert!(!classify_failure(429, "").needs_sign_in());
    // Only the code is quoted, never the body.
    let message = classify_failure(503, r#"{"error":"overloaded","detail":"secret-ish"}"#).to_string();
    assert!(message.contains("overloaded"));
    assert!(!message.contains("secret-ish"));
}

#[test]
fn a_login_too_long_for_one_security_command_does_not_fit_the_keychain() {
    use quota_core::claude::local::CredentialStore;

    let keychain = CredentialStore::Keychain {
        service: "Claude Code-credentials".to_string(),
        account: Some("someone".to_string()),
    };
    // The secret goes in hex, so each byte takes two characters of the
    // 4032-byte command `security -i` reads.
    assert!(keychain.fits(1_900));
    assert!(!keychain.fits(2_100));
    assert!(CredentialStore::File("/tmp/x/.credentials.json".into()).fits(1_000_000));
}

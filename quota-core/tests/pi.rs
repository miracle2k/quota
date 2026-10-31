//! Covers reading the logins the pi coding agent keeps in auth.json.

use quota_core::pi::{login, parse_antigravity_accounts, parse_object, PiCredential, PiError};

const AUTH: &str = r#"{
  "zai": { "type": "api_key", "key": "zai-secret" },
  "env-key": { "type": "api_key", "key": "$QUOTA_PI_TEST_KEY", "env": { "QUOTA_PI_TEST_KEY": "from-entry" } },
  "braced": { "type": "api_key", "key": "${QUOTA_PI_TEST_UNSET_VARIABLE}" },
  "command": { "type": "api_key", "key": "!op read secret" },
  "dollar": { "type": "api_key", "key": "$$literal" },
  "openai-codex": { "type": "oauth", "access": "a", "refresh": "r", "expires": 1790000000000, "accountId": "acct-1" },
  "antigravity": { "type": "oauth", "access": "g", "refresh": "gr", "expires": 1.79e12, "projectId": "p-1", "email": "me@example.com" },
  "odd": { "type": "magic" }
}"#;

#[test]
fn reads_api_keys_the_way_pi_resolves_them() {
    let auth = parse_object(AUTH).expect("parse");
    let key = |provider: &str| match login(&auth, provider) {
        Ok(PiCredential::ApiKey(key)) => Ok(key),
        Ok(other) => panic!("not a key: {:?}", other),
        Err(err) => Err(err),
    };
    assert_eq!(key("zai").expect("zai"), "zai-secret");
    assert_eq!(key("env-key").expect("env"), "from-entry");
    assert_eq!(key("dollar").expect("dollar"), "$literal");
    assert!(matches!(key("braced"), Err(PiError::NotFound)));
    assert!(matches!(key("command"), Err(PiError::CommandKey)));
    assert!(matches!(key("missing"), Err(PiError::NotFound)));
    assert!(matches!(login(&auth, "odd"), Err(PiError::Malformed(_))));
}

#[test]
fn reads_oauth_logins_with_their_extra_fields() {
    let auth = parse_object(AUTH).expect("parse");
    let Ok(PiCredential::OAuth(codex)) = login(&auth, "openai-codex") else {
        panic!("codex login");
    };
    assert_eq!(codex.access, "a");
    assert_eq!(codex.refresh.as_deref(), Some("r"));
    assert_eq!(codex.expires_ms, Some(1_790_000_000_000));
    assert_eq!(codex.extra_text("accountId").as_deref(), Some("acct-1"));
    assert!(codex.is_expired_at_ms(1_790_000_000_000, 0));
    assert!(!codex.is_expired_at_ms(1_789_000_000_000, 60_000));

    let Ok(PiCredential::OAuth(google)) = login(&auth, "antigravity") else {
        panic!("antigravity login");
    };
    // pi writes numbers the way JavaScript prints them.
    assert_eq!(google.expires_ms, Some(1_790_000_000_000));
    assert_eq!(
        google.extra_text("email").as_deref(),
        Some("me@example.com")
    );
    assert_eq!(google.extra_text("projectId").as_deref(), Some("p-1"));
}

#[test]
fn reads_every_antigravity_account_the_extension_linked() {
    let root = parse_object(
        r#"{"version":1,"activeAccountId":"a@example.com","accounts":{
            "a@example.com":{"access":"x","refresh":"y","expires":1,"email":"a@example.com"},
            "account-1234":{"access":"z","refresh":"w","expires":2,"email":"b@example.com"},
            "broken":{"refresh":"no access token"}
        }}"#,
    )
    .expect("parse");
    let mut emails: Vec<String> = parse_antigravity_accounts(&root)
        .iter()
        .filter_map(|account| account.extra_text("email"))
        .collect();
    emails.sort();
    assert_eq!(emails, vec!["a@example.com", "b@example.com"]);
}

#[test]
fn errors_never_quote_the_file() {
    let error = parse_object(r#"{"zai": {"key": "secret-value""#).expect_err("broken");
    assert!(!error.to_string().contains("secret-value"));
    let rendered = format!(
        "{:?}",
        login(&parse_object(AUTH).expect("parse"), "zai").expect("zai")
    );
    assert!(!rendered.contains("zai-secret"));
}

//! Covers the config file that lists extra Claude and Codex logins.

use quota_cli::config::{expand_home, load_from, parse, ClaudeAccount, CodexAccount, Config};

#[test]
fn parses_extra_claude_and_codex_logins() {
    let config = parse(
        r#"
        # Comments are allowed, which is the point of TOML here.
        [[claude]]
        label = "work"
        config_dir = "~/.claude-work"

        [[codex]]
        label = "personal"
        home = "~/.codex-personal"

        [[codex]]
        label = "client"
        home = "/srv/codex-client"
        "#,
    )
    .expect("parse");

    assert_eq!(
        config,
        Config {
            claude: vec![ClaudeAccount {
                label: "work".to_string(),
                config_dir: "~/.claude-work".to_string(),
            }],
            codex: vec![
                CodexAccount {
                    label: "personal".to_string(),
                    home: "~/.codex-personal".to_string(),
                },
                CodexAccount {
                    label: "client".to_string(),
                    home: "/srv/codex-client".to_string(),
                },
            ],
        }
    );
}

#[test]
fn an_empty_file_is_an_empty_config() {
    assert_eq!(parse(""), Ok(Config::default()));
}

#[test]
fn a_misspelled_key_is_rejected_rather_than_ignored() {
    // An ignored typo would silently drop the login it was meant to add.
    let error =
        parse("[[codex]]\nlabel = \"work\"\nhom = \"~/.codex-work\"\n").expect_err("should reject");
    assert!(
        error.contains("hom"),
        "error should name the key: {}",
        error
    );
}

#[test]
fn a_blank_label_is_rejected() {
    let error = parse("[[claude]]\nlabel = \" \"\nconfig_dir = \"~/.claude-work\"\n")
        .expect_err("should reject");
    assert!(error.contains("label"));
}

#[test]
fn a_missing_default_file_is_empty_but_a_missing_named_file_is_an_error() {
    let missing = std::env::temp_dir()
        .join(format!("quota-cli-config-{}", std::process::id()))
        .join("config.toml");

    assert_eq!(load_from(&missing, false), Ok(Config::default()));
    assert!(load_from(&missing, true).is_err());
}

#[test]
fn reports_the_file_and_position_of_a_syntax_error() {
    let dir = std::env::temp_dir().join(format!("quota-cli-config-bad-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let path = dir.join("config.toml");
    std::fs::write(&path, "[[codex]\nlabel = \"x\"\n").expect("write config");

    let error = load_from(&path, false).expect_err("should reject");
    assert!(error.contains(&path.display().to_string()));
}

#[test]
fn expands_a_leading_tilde_like_a_shell() {
    let home = dirs::home_dir().expect("home directory");
    let home = home.to_string_lossy();

    assert_eq!(
        expand_home("~/.codex-work"),
        format!("{}/.codex-work", home)
    );
    assert_eq!(expand_home("~"), home);
    assert_eq!(expand_home("/srv/codex"), "/srv/codex");
    // `~user` means another user's home, which this does not resolve.
    assert_eq!(expand_home("~other/.codex"), "~other/.codex");
}

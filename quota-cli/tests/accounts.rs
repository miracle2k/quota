//! Covers which logins the usage report lists when the config file adds some.

use quota_cli::accounts::{claude_login_for, unique_logins};

fn keys(items: &[Option<&str>]) -> Vec<Option<String>> {
    items.iter().map(|item| item.map(str::to_string)).collect()
}

#[test]
fn the_current_login_comes_first_when_no_entry_covers_it() {
    assert_eq!(
        unique_logins(&keys(&[Some("now"), Some("a"), Some("b")])),
        vec![0, 1, 2]
    );
}

#[test]
fn a_configured_entry_replaces_the_current_login_it_repeats() {
    // The provider's CLI is signed in to the account the file calls "b". It is
    // listed once, under the file's label, in the file's position.
    assert_eq!(
        unique_logins(&keys(&[Some("b"), Some("a"), Some("b")])),
        vec![1, 2]
    );
}

#[test]
fn a_repeated_configured_entry_keeps_only_the_first() {
    assert_eq!(
        unique_logins(&keys(&[Some("now"), Some("a"), Some("a")])),
        vec![0, 1]
    );
}

#[test]
fn logins_that_could_not_be_read_are_never_merged() {
    // Each one gets its own line, so each problem is visible.
    assert_eq!(unique_logins(&keys(&[None, None, None])), vec![0, 1, 2]);
    assert_eq!(unique_logins(&keys(&[None, Some("a")])), vec![0, 1]);
}

#[test]
fn naming_the_default_claude_directory_means_the_default_login() {
    // Claude Code keeps the default login under the plain Keychain item. The
    // hashed item only exists when CLAUDE_CONFIG_DIR is set.
    let home = dirs::home_dir().expect("home directory");
    let default_dir = home.join(".claude");

    let login = claude_login_for(&default_dir.to_string_lossy());
    assert_eq!(login.keychain_service(), "Claude Code-credentials");

    let with_slash = claude_login_for(&format!("{}/", default_dir.display()));
    assert_eq!(with_slash.keychain_service(), "Claude Code-credentials");

    let other = claude_login_for("/Users/test/.claude-work");
    assert_eq!(other.keychain_service(), "Claude Code-credentials-03abf0ee");
}

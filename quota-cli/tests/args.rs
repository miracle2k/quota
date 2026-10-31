use quota_cli::args::{
    parse_args, Command, DEFAULT_WATCH_INTERVAL_SECS, MIN_WATCH_INTERVAL_SECS,
};

fn argv(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| item.to_string()).collect()
}

#[test]
fn usage_defaults_to_human_output() {
    let parsed = parse_args(&argv(&["usage"])).expect("parse");
    assert_eq!(
        parsed,
        Command::Usage {
            json: false,
            all: false,
            providers: vec![]
        }
    );
}

#[test]
fn usage_accepts_the_json_flag() {
    let parsed = parse_args(&argv(&["usage", "--json"])).expect("parse");
    assert_eq!(
        parsed,
        Command::Usage {
            json: true,
            all: false,
            providers: vec![]
        }
    );
}

#[test]
fn usage_accepts_the_all_flag_alone_or_with_json() {
    assert_eq!(
        parse_args(&argv(&["usage", "--all"])),
        Ok(Command::Usage {
            json: false,
            all: true,
            providers: vec![]
        })
    );
    assert_eq!(
        parse_args(&argv(&["usage", "--all", "--json"])),
        Ok(Command::Usage {
            json: true,
            all: true,
            providers: vec![]
        })
    );
}

#[test]
fn no_arguments_asks_for_help() {
    assert_eq!(parse_args(&argv(&[])), Ok(Command::Help));
}

#[test]
fn an_unknown_command_is_an_error_naming_the_command() {
    let error = parse_args(&argv(&["wat"])).expect_err("should reject");
    assert!(error.contains("wat"));
}

#[test]
fn an_unknown_flag_is_an_error_naming_the_flag() {
    let error = parse_args(&argv(&["usage", "--nope"])).expect_err("should reject");
    assert!(error.contains("--nope"));
}

#[test]
fn herdr_report_parses() {
    let parsed = parse_args(&argv(&["herdr", "report"])).expect("parse");
    assert_eq!(parsed, Command::HerdrReport { force: false });
}

#[test]
fn herdr_report_accepts_force() {
    let parsed = parse_args(&argv(&["herdr", "report", "--force"])).expect("parse");
    assert_eq!(parsed, Command::HerdrReport { force: true });
}

#[test]
fn herdr_without_a_subcommand_is_an_error() {
    assert!(parse_args(&argv(&["herdr"])).is_err());
}

#[test]
fn herdr_watch_defaults_to_the_documented_interval() {
    let parsed = parse_args(&argv(&["herdr", "watch"])).expect("parse");
    assert_eq!(
        parsed,
        Command::HerdrWatch {
            interval_secs: DEFAULT_WATCH_INTERVAL_SECS
        }
    );
}

#[test]
fn herdr_watch_accepts_an_interval() {
    let parsed = parse_args(&argv(&["herdr", "watch", "--interval", "600"])).expect("parse");
    assert_eq!(parsed, Command::HerdrWatch { interval_secs: 600 });
}

#[test]
fn the_shortest_useful_interval_is_accepted() {
    let value = MIN_WATCH_INTERVAL_SECS.to_string();
    let parsed = parse_args(&argv(&["herdr", "watch", "--interval", &value])).expect("parse");
    assert_eq!(
        parsed,
        Command::HerdrWatch {
            interval_secs: MIN_WATCH_INTERVAL_SECS
        }
    );
}

#[test]
fn an_interval_inside_the_cache_ttl_is_rejected_with_the_reason() {
    let value = (MIN_WATCH_INTERVAL_SECS - 1).to_string();
    let error =
        parse_args(&argv(&["herdr", "watch", "--interval", &value])).expect_err("should reject");
    assert!(error.contains(&MIN_WATCH_INTERVAL_SECS.to_string()));
    assert!(error.contains("cache"));
}

#[test]
fn an_interval_that_is_not_a_number_is_an_error_naming_the_value() {
    let error =
        parse_args(&argv(&["herdr", "watch", "--interval", "soon"])).expect_err("should reject");
    assert!(error.contains("soon"));
}

#[test]
fn an_interval_flag_without_a_value_is_an_error() {
    assert!(parse_args(&argv(&["herdr", "watch", "--interval"])).is_err());
}

#[test]
fn an_unknown_watch_flag_is_an_error_naming_the_flag() {
    let error = parse_args(&argv(&["herdr", "watch", "--nope"])).expect_err("should reject");
    assert!(error.contains("--nope"));
}

#[test]
fn usage_can_name_the_providers_to_check() {
    assert_eq!(
        parse_args(&argv(&["usage", "codex", "claude", "codex", "--json"])),
        Ok(Command::Usage {
            json: true,
            all: false,
            providers: vec!["codex".to_string(), "claude".to_string()]
        })
    );
    let error = parse_args(&argv(&["usage", "gemini"])).expect_err("should reject");
    assert!(error.contains("gemini"));
}

#[test]
fn accounts_lists_by_default() {
    assert_eq!(
        parse_args(&argv(&["accounts"])),
        Ok(Command::AccountsList { json: false })
    );
    assert_eq!(
        parse_args(&argv(&["accounts", "--json"])),
        Ok(Command::AccountsList { json: true })
    );
    assert_eq!(
        parse_args(&argv(&["accounts", "list", "--json"])),
        Ok(Command::AccountsList { json: true })
    );
}

#[test]
fn accounts_add_and_remove_take_a_provider_and_a_label() {
    assert_eq!(
        parse_args(&argv(&["accounts", "add", "codex", "work", "--device-auth"])),
        Ok(Command::AccountsAdd {
            provider: "codex".to_string(),
            label: "work".to_string(),
            device_auth: true
        })
    );
    assert_eq!(
        parse_args(&argv(&["accounts", "add", "claude", " Client A "])),
        Ok(Command::AccountsAdd {
            provider: "claude".to_string(),
            label: "Client A".to_string(),
            device_auth: false
        })
    );
    assert_eq!(
        parse_args(&argv(&["accounts", "remove", "claude", "work"])),
        Ok(Command::AccountsRemove {
            provider: "claude".to_string(),
            label: "work".to_string()
        })
    );
}

#[test]
fn accounts_rejects_what_it_cannot_do() {
    for args in [
        &["accounts", "add", "cursor", "work"][..],
        &["accounts", "add", "codex"][..],
        &["accounts", "add", "codex", "  "][..],
        &["accounts", "add", "claude", "work", "--device-auth"][..],
        &["accounts", "remove", "codex", "work", "--device-auth"][..],
        &["accounts", "list", "extra"][..],
        &["accounts", "rename", "codex", "work"][..],
    ] {
        assert!(parse_args(&argv(args)).is_err(), "{:?}", args);
    }
}

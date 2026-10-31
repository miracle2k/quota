use crate::herdr::DEFAULT_TTL_MS;
use quota_core::providers::PROVIDERS;

/// How often `herdr watch` reports when the interval is not given.
pub const DEFAULT_WATCH_INTERVAL_SECS: u64 = 300;

/// The shortest interval worth running. A cycle inside the cache TTL serves the
/// value it already has and makes no network request, so a shorter interval
/// would burn wakeups without ever producing a newer number.
pub const MIN_WATCH_INTERVAL_SECS: u64 = (DEFAULT_TTL_MS / 1000) as u64;

/// The providers that can have more than one account.
pub const ACCOUNT_PROVIDERS: &[&str] = &["claude", "codex"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// `providers` empty means every provider.
    Usage {
        json: bool,
        all: bool,
        providers: Vec<String>,
    },
    AccountsList {
        json: bool,
    },
    AccountsAdd {
        provider: String,
        label: String,
        device_auth: bool,
    },
    AccountsRemove {
        provider: String,
        label: String,
    },
    HerdrReport {
        force: bool,
    },
    HerdrWatch {
        interval_secs: u64,
    },
    Help,
}

pub const HELP: &str = "\
quota-cli — read AI usage from locally stored agent CLI credentials

Usage:
  quota-cli usage [--json] [--all] [PROVIDER...]
                                     Print current usage, for some providers only
                                     when named (claude, codex, cursor, ...)
  quota-cli accounts [list] [--json] List every Claude and Codex login found
  quota-cli accounts add claude|codex LABEL [--device-auth]
                                     Sign in to another account and keep it
  quota-cli accounts remove claude|codex LABEL
                                     Delete an account that `accounts add` made
  quota-cli herdr report             Push usage tokens into the Herdr sidebar
  quota-cli herdr watch              Keep reporting on an interval until stopped
  quota-cli --help                   Show this help

Options for usage:
  --json   Print one JSON record per account, including the ones not set up
  --all    Also list providers with no login here, and the reason

Claude and Codex accounts are found in several places: the login each CLI
uses now, the accounts `accounts add` made, the config file, CodexBar and
claude-swap. Each account is shown once.

quota-cli keeps the accounts it adds in its data directory (on macOS
~/Library/Application Support/quota-cli; QUOTA_CLI_DATA_DIR moves it).
The config file is ~/.config/quota-cli/config.toml (QUOTA_CLI_CONFIG moves it).
See the README for its format.
";

pub fn parse_args(argv: &[String]) -> Result<Command, String> {
    let Some(first) = argv.first() else {
        return Ok(Command::Help);
    };

    match first.as_str() {
        "--help" | "-h" | "help" => Ok(Command::Help),
        "usage" => {
            let mut json = false;
            let mut all = false;
            let mut providers: Vec<String> = Vec::new();
            for arg in &argv[1..] {
                match arg.as_str() {
                    "--json" => json = true,
                    "--all" => all = true,
                    flag if flag.starts_with('-') => return Err(format!("unknown flag: {}", flag)),
                    name if PROVIDERS.contains(&name) => {
                        if !providers.iter().any(|known| known == name) {
                            providers.push(name.to_string());
                        }
                    }
                    other => {
                        return Err(format!(
                            "unknown provider: {} (known: {})",
                            other,
                            PROVIDERS.join(", ")
                        ))
                    }
                }
            }
            Ok(Command::Usage {
                json,
                all,
                providers,
            })
        }
        "accounts" => parse_accounts(&argv[1..]),
        "herdr" => match argv.get(1).map(String::as_str) {
            Some("report") => {
                let mut force = false;
                for flag in &argv[2..] {
                    match flag.as_str() {
                        "--force" => force = true,
                        other => return Err(format!("unknown flag: {}", other)),
                    }
                }
                Ok(Command::HerdrReport { force })
            }
            Some("watch") => parse_watch(&argv[2..]),
            Some(other) => Err(format!("unknown herdr subcommand: {}", other)),
            None => Err("herdr needs a subcommand: report or watch".to_string()),
        },
        other => Err(format!("unknown command: {}", other)),
    }
}

fn parse_accounts(args: &[String]) -> Result<Command, String> {
    let (action, rest) = match args.split_first() {
        Some((first, rest)) if !first.starts_with('-') => (first.as_str(), rest),
        _ => ("list", args),
    };
    let mut flags = Vec::new();
    let mut positional = Vec::new();
    for arg in rest {
        if arg.starts_with("--") {
            flags.push(arg.as_str());
        } else {
            positional.push(arg.as_str());
        }
    }
    let unknown_flag = |allowed: &[&str]| {
        flags
            .iter()
            .find(|flag| !allowed.contains(flag))
            .map(|flag| format!("unknown flag: {}", flag))
    };

    match action {
        "list" | "ls" => {
            if let Some(error) = unknown_flag(&["--json"]) {
                return Err(error);
            }
            if let Some(extra) = positional.first() {
                return Err(format!("accounts list takes no arguments, got: {}", extra));
            }
            Ok(Command::AccountsList {
                json: flags.contains(&"--json"),
            })
        }
        "add" | "remove" | "rm" => {
            let allowed: &[&str] = if action == "add" {
                &["--device-auth"]
            } else {
                &[]
            };
            if let Some(error) = unknown_flag(allowed) {
                return Err(error);
            }
            let [provider, label] = positional[..] else {
                return Err(format!(
                    "accounts {} needs a provider (claude or codex) and a label",
                    action
                ));
            };
            if !ACCOUNT_PROVIDERS.contains(&provider) {
                return Err(format!(
                    "accounts can only be added for claude or codex, not {}",
                    provider
                ));
            }
            if label.trim().is_empty() {
                return Err("the label must not be empty".to_string());
            }
            if provider == "claude" && flags.contains(&"--device-auth") {
                return Err(
                    "--device-auth is for codex; Claude Code prints a link instead".to_string(),
                );
            }
            let (provider, label) = (provider.to_string(), label.trim().to_string());
            Ok(if action == "add" {
                Command::AccountsAdd {
                    provider,
                    label,
                    device_auth: flags.contains(&"--device-auth"),
                }
            } else {
                Command::AccountsRemove { provider, label }
            })
        }
        other => Err(format!("unknown accounts subcommand: {}", other)),
    }
}

fn parse_watch(flags: &[String]) -> Result<Command, String> {
    let mut interval_secs = DEFAULT_WATCH_INTERVAL_SECS;
    let mut rest = flags.iter();

    while let Some(flag) = rest.next() {
        match flag.as_str() {
            "--interval" => {
                let value = rest
                    .next()
                    .ok_or_else(|| "--interval needs a number of seconds".to_string())?;
                interval_secs = value
                    .parse::<u64>()
                    .map_err(|_| format!("--interval needs a number of seconds, got: {}", value))?;
            }
            other => return Err(format!("unknown flag: {}", other)),
        }
    }

    if interval_secs < MIN_WATCH_INTERVAL_SECS {
        return Err(format!(
            "--interval must be at least {} seconds, because a report inside the {} second cache TTL serves the cached value and fetches nothing",
            MIN_WATCH_INTERVAL_SECS, MIN_WATCH_INTERVAL_SECS
        ));
    }

    Ok(Command::HerdrWatch { interval_secs })
}

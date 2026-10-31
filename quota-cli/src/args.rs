use crate::herdr::DEFAULT_TTL_MS;

/// How often `herdr watch` reports when the interval is not given.
pub const DEFAULT_WATCH_INTERVAL_SECS: u64 = 300;

/// The shortest interval worth running. A cycle inside the cache TTL serves the
/// value it already has and makes no network request, so a shorter interval
/// would burn wakeups without ever producing a newer number.
pub const MIN_WATCH_INTERVAL_SECS: u64 = (DEFAULT_TTL_MS / 1000) as u64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Usage { json: bool, all: bool },
    HerdrReport { force: bool },
    HerdrWatch { interval_secs: u64 },
    Help,
}

pub const HELP: &str = "\
quota-cli — read AI usage from locally stored agent CLI credentials

Usage:
  quota-cli usage [--json] [--all]   Print current usage
  quota-cli herdr report             Push usage tokens into the Herdr sidebar
  quota-cli herdr watch              Keep reporting on an interval until stopped
  quota-cli --help                   Show this help

Options for usage:
  --json   Print one JSON record per login, including the ones not set up
  --all    Also list providers with no login here, and the reason

Extra Claude and Codex logins go in ~/.config/quota-cli/config.toml
(or the file QUOTA_CLI_CONFIG names). See the README for the format.
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
            for flag in &argv[1..] {
                match flag.as_str() {
                    "--json" => json = true,
                    "--all" => all = true,
                    other => return Err(format!("unknown flag: {}", other)),
                }
            }
            Ok(Command::Usage { json, all })
        }
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

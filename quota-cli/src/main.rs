use quota_cli::args::{parse_args, Command, HELP};

#[tokio::main]
async fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();

    let command = match parse_args(&argv) {
        Ok(command) => command,
        Err(message) => {
            eprintln!("{}", message);
            eprintln!("\n{}", HELP);
            std::process::exit(2);
        }
    };

    match command {
        Command::Help => println!("{}", HELP),
        Command::Usage {
            json,
            all,
            providers,
        } => run_usage(json, all, &providers).await,
        Command::AccountsList { json } => run_accounts_list(json).await,
        Command::AccountsAdd {
            provider,
            label,
            device_auth,
        } => finish(quota_cli::manage::add(&provider, &label, device_auth)),
        Command::AccountsRemove { provider, label } => {
            finish(quota_cli::manage::remove(&provider, &label))
        }
        Command::HerdrReport { force } => run_herdr_report(force).await,
        Command::HerdrWatch { interval_secs } => run_herdr_watch(interval_secs).await,
    }
}

/// Print a command's outcome and exit non-zero when it failed.
fn finish(result: Result<String, String>) {
    match result {
        Ok(message) => println!("{}", message),
        Err(message) => {
            eprintln!("{}", message);
            std::process::exit(1);
        }
    }
}

/// The config file and quota-cli's own account list. Either one being broken
/// is a mistake worth stopping for, since an account would silently go missing.
fn load_sources() -> (quota_cli::config::Config, quota_cli::store::Registry) {
    let config = match quota_cli::config::load() {
        Ok(config) => config,
        Err(message) => {
            eprintln!("config: {}", message);
            std::process::exit(2);
        }
    };
    let registry = match quota_cli::store::data_dir() {
        Some(dir) => match quota_cli::store::load(&dir) {
            Ok(registry) => registry,
            Err(message) => {
                eprintln!("accounts: {}", message);
                std::process::exit(2);
            }
        },
        None => Default::default(),
    };
    (config, registry)
}

async fn run_accounts_list(json: bool) {
    use quota_cli::render;

    let (config, registry) = load_sources();
    let accounts = quota_cli::accounts::list_accounts(&config, &registry).await;
    if json {
        let records: Vec<_> = accounts.iter().map(render::listed_record).collect();
        match serde_json::to_string_pretty(&records) {
            Ok(rendered) => println!("{}", rendered),
            Err(err) => {
                eprintln!("could not render JSON: {}", err);
                std::process::exit(1);
            }
        }
        return;
    }
    print!("{}", render::account_list(&accounts));
    if let Some(dir) = quota_cli::store::data_dir() {
        println!(
            "\nAccounts added with `quota-cli accounts add` live in {}",
            render::shorten_home(&dir.display().to_string())
        );
    }
}

async fn run_usage(json: bool, all: bool, providers: &[String]) {
    use quota_cli::render;

    let (config, registry) = load_sources();
    // Providers named on the command line are checked even when the config
    // file skips them.
    let providers: Vec<&'static str> = quota_core::providers::PROVIDERS
        .iter()
        .copied()
        .filter(|known| {
            if providers.is_empty() {
                !config.usage.skip.iter().any(|skipped| skipped == known)
            } else {
                providers.iter().any(|wanted| wanted == known)
            }
        })
        .collect();

    let reports = quota_cli::accounts::fetch_all(&config, &registry, &providers).await;
    let any_succeeded = reports.iter().any(|report| report.result.is_ok());

    if json {
        // Every login checked is a record, including the ones that failed or
        // are not set up, so a script can tell which is which from `status`.
        let records: Vec<_> = reports.iter().map(render::usage_record).collect();
        match serde_json::to_string_pretty(&records) {
            Ok(rendered) => println!("{}", rendered),
            Err(err) => {
                eprintln!("could not render JSON: {}", err);
                std::process::exit(1);
            }
        }
    } else {
        // One clock reading for the whole report, so two windows that reset at
        // the same moment cannot render as different durations.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs() as i64)
            .unwrap_or(0);

        let shown: Vec<_> = reports
            .iter()
            .filter(|report| {
                render::shown_in_text(
                    report,
                    now,
                    all,
                    config.usage.hides_errors_of(report.provider),
                )
            })
            .collect();

        if shown.is_empty() {
            eprintln!(
                "No provider has a login here. Run `quota-cli usage --all` to see each reason."
            );
        } else {
            print!(
                "{}",
                render::usage_report(&shown, now, render::colors_enabled())
            );
        }
    }

    // Any provider reporting is a useful run. Exiting non-zero because one
    // provider the user never signed into failed would make the command look
    // broken on a machine where it is working fine.
    if !any_succeeded {
        std::process::exit(1);
    }
}

/// Why a single report produced no numbers.
enum ReportFailure {
    /// Herdr itself could not be asked what is on screen. Nothing was printed,
    /// so the caller decides whether this is worth saying out loud.
    Herdr(String),
    /// Panes wanted numbers and none of them got one. The reasons were already
    /// printed as they happened.
    NothingReported,
}

/// Run the report once and exit the way the command always has: any failure is
/// a non-zero exit, and a Herdr failure names itself on stderr first.
async fn run_herdr_report(force: bool) {
    if let Err(failure) = herdr_report_once(force).await {
        if let ReportFailure::Herdr(message) = failure {
            eprintln!("{}", message);
        }
        std::process::exit(1);
    }
}

/// Report on an interval until the process is stopped.
///
/// Herdr has no periodic event, so nothing inside Herdr can drive this. The
/// loop deliberately does not force a fetch: the interval is longer than the
/// cache TTL, so the cache is already stale by the time each cycle runs, and a
/// forced fetch would only add requests when something else refreshed in
/// between.
async fn run_herdr_watch(interval_secs: u64) {
    let interval = std::time::Duration::from_secs(interval_secs);

    loop {
        // A failed cycle is never fatal here. Herdr can be restarted, and a
        // provider can be signed back into, without the watcher being restarted
        // along with it.
        if let Err(ReportFailure::Herdr(message)) = herdr_report_once(false).await {
            eprintln!("{}", message);
        }
        tokio::time::sleep(interval).await;
    }
}

async fn herdr_report_once(force: bool) -> Result<(), ReportFailure> {
    use quota_cli::herdr;
    use std::collections::BTreeMap;

    // Check which panes need a number before touching the cache or the
    // network. A user running no agent we can report on should never fetch,
    // and never log an error, on every agent-status event.
    let panes = herdr::list_agent_panes().map_err(ReportFailure::Herdr)?;

    let reportable_panes: Vec<_> = panes
        .into_iter()
        .filter_map(|pane| {
            herdr::provider_for_agent_kind(&pane.agent).map(|provider| (pane, provider))
        })
        .collect();

    if reportable_panes.is_empty() {
        return Ok(());
    }

    let state_dir = std::env::var("HERDR_PLUGIN_STATE_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir().join("quota-herdr"));

    let source = format!(
        "plugin:{}",
        std::env::var("HERDR_PLUGIN_ID").unwrap_or_else(|_| "pinkpixel.quota".to_string())
    );

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(0);

    // Only the providers actually on screen. A signed-out provider with no pane
    // must not cost a request, or an error, on every status change.
    let mut needed: Vec<&'static str> = Vec::new();
    for (_, provider) in &reportable_panes {
        if !needed.contains(provider) {
            needed.push(provider);
        }
    }

    let mut tokens: BTreeMap<&'static str, String> = BTreeMap::new();
    for provider in needed {
        if let Some(token) = token_for_provider(provider, &state_dir, now_ms, force).await {
            tokens.insert(provider, token);
        }
    }

    let mut any_report_succeeded = false;

    for (pane, provider) in &reportable_panes {
        let Some(token) = tokens.get(provider) else {
            continue;
        };
        match herdr::report_pane_token(&pane.pane_id, &source, token) {
            Ok(()) => any_report_succeeded = true,
            Err(err) => eprintln!("{}", err),
        }
    }

    // The workspace row carries no agent name, so it can only be reported when
    // every reportable pane in that workspace runs the same provider. A mixed
    // workspace gets no row rather than one agent's number labelled as all.
    let mut workspace_providers: BTreeMap<&str, Option<&'static str>> = BTreeMap::new();
    for (pane, provider) in &reportable_panes {
        workspace_providers
            .entry(pane.workspace_id.as_str())
            .and_modify(|existing| {
                if *existing != Some(*provider) {
                    *existing = None;
                }
            })
            .or_insert(Some(*provider));
    }

    for (workspace_id, provider) in workspace_providers {
        let Some(token) = provider.and_then(|provider| tokens.get(provider)) else {
            continue;
        };
        match herdr::report_workspace_token(workspace_id, &source, token) {
            Ok(()) => any_report_succeeded = true,
            Err(err) => eprintln!("{}", err),
        }
    }

    if any_report_succeeded {
        Ok(())
    } else {
        Err(ReportFailure::NothingReported)
    }
}

/// Resolve one provider's display token, preferring a fresh cache entry.
///
/// A failed fetch falls back to the last good value rather than blanking the
/// pane, matching what the single-provider version did.
async fn token_for_provider(
    provider: &str,
    state_dir: &std::path::Path,
    now_ms: i64,
    force: bool,
) -> Option<String> {
    use quota_cli::herdr;

    let cached = herdr::read_cache(state_dir, provider);
    let needs_fetch = force
        || cached
            .as_ref()
            .map(|cache| cache.is_stale_at_ms(now_ms, herdr::DEFAULT_TTL_MS))
            .unwrap_or(true);

    if !needs_fetch {
        return cached.map(|cache| cache.token);
    }

    match quota_core::providers::fetch_provider(provider).await {
        Some(Ok(usage)) => match usage.compact_token() {
            Some(token) => {
                let cache = herdr::TokenCache {
                    fetched_at_ms: now_ms,
                    token: token.clone(),
                };
                if let Err(err) = herdr::write_cache(state_dir, provider, &cache) {
                    eprintln!("{}", err);
                }
                Some(token)
            }
            None => {
                eprintln!("{} usage returned no numbers to show", provider);
                cached.map(|cache| cache.token)
            }
        },
        Some(Err(message)) => {
            eprintln!("{}: {}", provider, message);
            cached.map(|cache| cache.token)
        }
        None => {
            eprintln!("{} has no usage reader", provider);
            None
        }
    }
}

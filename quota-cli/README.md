# quota-cli

Read your AI usage from the credentials your agent CLIs already stored.

`quota-cli` prints how much quota you have left across Claude, Codex, Cursor, Antigravity, Grok, Kiro, and Z.ai. It reads each provider's token from wherever that provider's own CLI keeps it, so there is no sign-in step and no account to connect. If you are logged into Claude Code, you get Claude numbers. If you are not, Claude is left out of the report.

Claude, Codex, Antigravity and Z.ai can have more than one account: `quota-cli accounts add` signs in to another Claude or Codex one, and the accounts that [CodexBar](https://github.com/steipete/CodexBar), [claude-swap](https://github.com/realiti4/claude-swap) and the [pi](https://github.com/earendil-works/pi) coding agent hold are picked up as well. See [More accounts](#more-accounts).

It is part of [Quota](https://github.com/pinkpixel-dev/quota), a desktop app that does the same thing with a UI. The CLI is standalone and does not need the app installed or running.

## Install

```bash
cargo install quota-cli
```

Kiro's credentials live in a SQLite database, which is compiled from bundled C source, so you need a working C compiler for the build.

## Usage

```bash
quota-cli usage
```

```text
Claude  default  me@example.com
        5h 98% left · resets in 4h 58m
        Wk 35% left · resets in 3d 9h

Claude  work     work@example.com
        Wk 12% left · resets in 2d 4h
        1 reset available · expires in 9d 2h

Codex   default  me@example.com
        5h 100% left · resets in 4h 59m
        Wk 95% left · resets in 4d 6h

Codex   client   client@example.com
        5h 0% left · resets in 1h 10m
        Wk 60% left · resets in 4d 6h
        2 resets available · next expires in 6d 4h

Kiro    default  me@example.com
        Credits 100% left
        Bonus 100% left

Z.ai    default  email unavailable
        Balance $7.95 · No GLM Coding Plan
```

The first line identifies the provider, account label, and email. Each quota window and banked reset has its own indented line, so long reports do not wrap into each other. Percentages are what you have left, not what you have used. Reset times show when a provider reports one, and Kiro currently does not. Banked limit resets show when an account has at least one, see [Limit resets](#limit-resets).

Interactive terminals use color: remaining quota is green, yellow, or red; account details and timing are dimmed. Redirected output stays plain. Set `NO_COLOR` or `CLICOLOR=0` to disable color, or `CLICOLOR_FORCE=1` to force it.

Providers you are not signed into are left out, and so is a login with nothing to show. `--all` lists them too, with the reason:

```bash
quota-cli usage --all
```

Name providers to check only those:

```bash
quota-cli usage claude codex
```

To leave a provider out for good, list it in the config file (see [The config file](#the-config-file)). `skip` means it is never checked, unless you name it on the command line. `hide_errors` means it shows only when it reports usage, and its errors show only with `--all` or in `--json`:

```toml
[usage]
skip = ["kiro"]
hide_errors = ["antigravity"]
```

For scripting, `--json` prints one record per account, including the ones `--all` would add:

```bash
quota-cli usage --json
```

```json
[
  {
    "provider": "codex",
    "label": "client",
    "email": "client@example.com",
    "sources": ["quota-cli", "codexbar"],
    "status": "ok",
    "accountLabel": "pro",
    "windows": [
      { "label": "5h", "remainingPercent": 0, "resetAt": 1793462400 },
      { "label": "Wk", "remainingPercent": 60, "resetAt": 1793836800 }
    ],
    "note": null,
    "resets": { "available": 2, "expiresAt": 1794067200 },
    "error": null
  },
  {
    "provider": "cursor",
    "label": null,
    "email": null,
    "sources": ["current"],
    "status": "not_configured",
    "accountLabel": null,
    "windows": [],
    "note": null,
    "resets": null,
    "error": "No Cursor credentials found. Sign in with the Cursor CLI first."
  }
]
```

Every record has every field. `status` is `ok`, `not_configured` (no login here, or one with no usage to measure), or `error`. `label` is the name the account is shown under: a label you gave it, a claude-swap alias, or its email address, and `null` for the login the provider's CLI uses now. `sources` lists every place the account was found: `current`, `quota-cli`, `config`, `codexbar`, `claude-swap`. `resets` is `null` when the provider has no such thing or did not say.

The command exits non-zero only when no login at all could report, since one signed-out provider on an otherwise working machine is not a failure.

## More accounts

`usage` reports every Claude, Codex, Antigravity and Z.ai account it can find, from these places:

1. The login each provider's CLI uses now: `~/.claude` (or `CLAUDE_CONFIG_DIR`), `~/.codex` (or `CODEX_HOME`), Antigravity's own sign-in, and for Z.ai the key in `ZAI_API_KEY`.
2. Accounts you added with `quota-cli accounts add`.
3. The config file.
4. CodexBar's Codex accounts, and the extra Codex homes its config file lists (`codexProfileHomePaths`).
5. claude-swap's Claude accounts, when `cswap` is installed.
6. The logins the pi coding agent keeps in `~/.pi/agent/auth.json` (`PI_CODING_AGENT_DIR` moves it): Claude, Codex, the Antigravity accounts of the [pi-antigravity](https://github.com/Rahularya01/pi-antigravity) extension, and Z.ai keys.

The same account often turns up in more than one place, so each account is shown once. Claude accounts are matched by email address and organization, the identity claude-swap uses too, Codex accounts by workspace and email, Antigravity accounts by Google address, and Z.ai accounts by key. The name is the most specific one any copy has: your label first, then nothing for the login the CLI uses now, then a claude-swap alias, then the email address. The numbers come from the copy whose token was renewed last.

See what was found, and where:

```bash
quota-cli accounts
```

```text
claude
  current      me@example.com      ~/.claude
claude (work)
  quota-cli    me@work.example     ~/Library/Application Support/quota-cli/accounts/claude/work
codex
  current      me@example.com      ~/.codex
  codexbar     me@example.com      ~/Library/Application Support/CodexBar/managed-codex-homes/…
```

`--json` prints the same as records.

### Adding an account

```bash
quota-cli accounts add claude work
quota-cli accounts add codex client [--device-auth]
```

This makes a directory for the account under quota-cli's data directory, `~/Library/Application Support/quota-cli` on macOS and `~/.local/share/quota-cli` on Linux (`QUOTA_CLI_DATA_DIR` moves it), and runs the provider's own sign-in pointed at it: `claude auth login` with `CLAUDE_CONFIG_DIR` set, or `codex login` with `CODEX_HOME` set. The result is an ordinary login, so you can also work in that account:

```bash
CLAUDE_CONFIG_DIR="$HOME/Library/Application Support/quota-cli/accounts/claude/work" claude
```

Running `accounts add` again with a label that exists signs in to that account again in the same directory, which is the fix for a login that cannot be renewed any more. `accounts remove claude work` deletes the directory and, on macOS, the Keychain item Claude Code made for it. Accounts from the config file, CodexBar or claude-swap are removed in those places.

### The config file

A login you keep somewhere else can be listed in `~/.config/quota-cli/config.toml` (or under `$XDG_CONFIG_HOME`, or in the file `QUOTA_CLI_CONFIG` names), each with a label for the report:

```toml
[[claude]]
label = "work"
config_dir = "~/.claude-work"

[[codex]]
label = "client"
home = "~/.codex-client"
```

Sign in once per directory with `CLAUDE_CONFIG_DIR=~/.claude-work claude auth login` or `CODEX_HOME=~/.codex-client codex login`. Write `config_dir` the way you set `CLAUDE_CONFIG_DIR`. On macOS, Claude Code names the Keychain item after that exact text, so `~/.claude-work` and `~/.claude-work/` are two different items. Naming the default `~/.claude` means the default login.

An account you added or listed in the file always shows, even when it cannot be read, so a mistake does not go unnoticed. An unknown key in the file is an error for the same reason.

The file can also turn off reading other tools' accounts:

```toml
[sources]
codexbar = false
claude_swap = false
```

And it can quiet providers you do not use, as shown under [Usage](#usage):

```toml
[usage]
skip = ["kiro"]                 # never checked
hide_errors = ["antigravity"]   # shown only when it reports usage
```

A provider name the file does not know is an error, so a typo does not silently quiet nothing.

### CodexBar, claude-swap and pi

CodexBar keeps each Codex account it manages in a Codex home of its own, listed in `~/Library/Application Support/CodexBar/managed-codex-accounts.json`. Each one is an ordinary Codex login, so quota-cli reads it, and renews its token when needed, like any other.

claude-swap keeps each Claude account's login in a store of its own and renews those tokens itself. A Claude refresh token works only once, so quota-cli never touches them: it runs `cswap --list --json` and shows the usage claude-swap measured. These accounts show no limit resets, because claude-swap does not report them.

pi renews its own logins under a lock of its own. quota-cli uses pi's Claude and Codex tokens while they are good and never renews them, since a refresh token spent outside pi would leave pi with a dead one. An expired one counts as not set up until pi renews it the next time it uses that provider (`--all` lists it). pi's Antigravity logins are Google logins, which can be renewed without replacing the refresh token, so quota-cli renews those in memory as it does Antigravity's own. A pi key that is a shell command (`!cmd`) is not run.

The Herdr plugin keeps reporting the login each CLI is using now. A pane does not say which account its agent was started with.

## Z.ai

Z.ai (GLM) reports two things for an API key. With a GLM Coding Plan, the plan's windows: `5h` and `Wk` for prompts, and `MCP` for the monthly allowance of web search and reader calls. With or without a plan, the prepaid balance, in US dollars for `api.z.ai` and in yuan for BigModel (pi's `zai-coding-cn`).

## Expired logins

A Claude access token lasts about eight hours, and a Codex token about ten days. A login nobody used in that time would report nothing, so `usage` renews an expired token before it asks for usage, and once more if the usage API turns a token down. It does this the way the provider's own CLI does, so the CLI picks up the new token instead of tripping over it:

- **Claude.** quota-cli takes the two lock directories Claude Code takes while it renews (`.oauth_refresh.lock` in the config directory, and `<config directory>.lock` beside it), reads the login again, and writes the result only if the stored refresh token is still the one it sent. The renewed login goes back where it came from: the Keychain item, through `/usr/bin/security` as Claude Code writes it, or `.credentials.json`.
- **Codex.** quota-cli renews against the issuer the login records, then writes `auth.json` under the `auth.lock` and `auth.json.lock` locks the CLI takes, only if the refresh token is unchanged, keeping every other field. If the CLI spent the same refresh token a moment earlier, quota-cli uses the CLI's result.

A refresh token works only once, so quota-cli checks everything that could stop it from saving the result before it sends one. If saving still fails, the renewed login is kept in `<file>.quota-renewed` beside the store, and the report says where. Only an HTTP 401 from the usage API counts as a rejected token; a 403 is reported as a refused request and renews nothing.

When the provider refuses to renew, the report says so and names the command that signs that account in again.

## Limit resets

Codex and Claude can each bank one-time resets that refill a usage window when you choose, instead of when it rolls over. `usage` shows how many an account has, and when the next one expires if the provider says.

- Codex reports the count in the same usage call, as `rate_limit_reset_credits`. When the count is above zero, `quota-cli` reads the list of resets (`GET /wham/rate-limit-reset-credits`) for their expiry dates.
- Claude reports resets per grant, with an end date, in the usage call when it asks for them (`cedar_ember=1`). The API only answers that part for a client whose user agent starts with `claude-cli/`, so the Claude usage call sends `claude-cli/<version> (external, cli) quota/<version>`. Any other user agent gets `ineligible_reason: "surface"`, which `quota-cli` treats as unknown, not as zero.

`quota-cli` only reads these. Using a reset stays with the provider's own CLI.

## Herdr plugin

```bash
quota-cli herdr report [--force]
```

This pushes usage into the [Herdr](https://herdr.dev) sidebar as a metadata token, so each pane shows the numbers for the agent it is actually running. It is meant to be driven by the plugin in [`herdr-plugin/`](https://github.com/pinkpixel-dev/quota/tree/main/herdr-plugin) rather than run by hand. Only providers with a pane on screen are fetched, and without `--force` each one serves a cached value until that cache goes stale.

Herdr has no periodic event, so nothing inside Herdr can keep those numbers moving on its own. `watch` is the answer to that:

```bash
quota-cli herdr watch [--interval SECONDS]
```

It reports, sleeps, and reports again until you stop it. The interval defaults to 300 seconds and cannot go below 120, because a cycle inside the cache TTL would serve the value it already has and fetch nothing new. It does not force a fetch, since the cache is stale by the time each cycle runs anyway.

A failed cycle is not fatal. If Herdr is not running, or a provider is signed out, the reason goes to stderr and the next cycle still happens. That does mean a persistent problem repeats in the log once per interval.

## Where the credentials come from

| Provider | Source |
|---|---|
| Claude | The macOS Keychain, or `.credentials.json` in `CLAUDE_CONFIG_DIR` (default `~/.claude`) where there is no Keychain item |
| Codex | `~/.codex/auth.json`, honoring `CODEX_HOME` |
| Cursor | `~/.config/cursor/auth.json` |
| Antigravity | OS keyring, falling back to `~/.gemini/oauth_creds.json` |
| Grok | `~/.grok/auth.json`, honoring `GROK_HOME` |
| Kiro | `~/.local/share/kiro-cli/data.sqlite3`, falling back to the AWS SSO cache |
| Z.ai | `ZAI_API_KEY`, or pi's `zai` / `zai-coding-cn` key |

Three of these are worth knowing about. Cursor, Kiro, and Antigravity keep separate credentials for their CLI and their IDE, and the two stores can hold different accounts or lapse independently. `quota-cli` reads the CLI store first, because that is the session an agent in a terminal actually uses.

## What it does not do

Apart from renewing expired Claude and Codex tokens as described in [Expired logins](#expired-logins), nothing is refreshed, rewritten, or rotated, because those tokens back your own agent sessions and rotating one carelessly could leave your real CLI holding a dead credential. claude-swap's tokens are never touched. Antigravity is renewed only in memory, because Google does not rotate its refresh token: a fresh access token is held for one request and nothing is written to disk. pi's logins are only read.

The practical effect shows up with Kiro. Its CLI refreshes its token only when it has a reason to call the service, so an idle machine holds a lapsed token and `quota-cli` reports that instead of a number. Using Kiro refreshes it.

Tokens are never logged or printed. Each credential struct has a hand-written `Debug` that redacts, and parse failures report only a line and column, because serde's own error message can quote the input it failed on.

## License

Apache-2.0. Made with 💖 by Pink Pixel.

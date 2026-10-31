# quota-cli

Read your AI usage from the credentials your agent CLIs already stored.

`quota-cli` prints how much quota you have left across Claude, Codex, Cursor, Antigravity, Grok, and Kiro. It reads each provider's token from wherever that provider's own CLI keeps it, so there is no sign-in step and no account to connect. If you are logged into Claude Code, you get Claude numbers. If you are not, Claude is left out of the report. An optional config file adds more Claude and Codex logins.

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
claude          5h 98% left (resets in 4h 58m) · Wk 35% left (resets in 3d 9h)
claude (work)   Wk 12% left (resets in 2d 4h) · 1 reset available (expires in 9d 2h)
codex           5h 100% left (resets in 4h 59m) · Wk 95% left (resets in 4d 6h)
codex (client)  5h 0% left (resets in 1h 10m) · Wk 60% left (resets in 4d 6h) · 2 resets available
kiro            Credits 100% left · Bonus 100% left
```

Percentages are what you have left, not what you have used. Reset times show when a provider reports one, and Kiro currently does not. Banked limit resets show when an account has at least one, see [Limit resets](#limit-resets).

Providers you are not signed into are left out, and so is a login with nothing to show. `--all` lists them too, with the reason:

```bash
quota-cli usage --all
```

For scripting, `--json` prints one record per login, including the ones `--all` would add:

```bash
quota-cli usage --json
```

```json
[
  {
    "provider": "codex",
    "label": "client",
    "status": "ok",
    "accountLabel": "pro",
    "windows": [
      { "label": "5h", "remainingPercent": 0, "resetAt": 1793462400 },
      { "label": "Wk", "remainingPercent": 60, "resetAt": 1793836800 }
    ],
    "note": null,
    "resets": { "available": 2, "expiresAt": null },
    "error": null
  },
  {
    "provider": "cursor",
    "label": null,
    "status": "not_configured",
    "accountLabel": null,
    "windows": [],
    "note": null,
    "resets": null,
    "error": "No Cursor credentials found. Sign in with the Cursor CLI first."
  }
]
```

Every record has every field. `status` is `ok`, `not_configured` (no login here, or one with no usage to measure), or `error`. `label` is the label from the config file, and `null` for a login found the usual way. `resets` is `null` when the provider has no such thing or did not say.

The command exits non-zero only when no login at all could report, since one signed-out provider on an otherwise working machine is not a failure.

## More Claude and Codex accounts

Claude Code and the Codex CLI can each keep a login in a separate directory: `CLAUDE_CONFIG_DIR` for Claude Code, `CODEX_HOME` for Codex. Sign in once per directory:

```bash
CLAUDE_CONFIG_DIR=~/.claude-work claude     # then /login
CODEX_HOME=~/.codex-client codex login
```

Then list those directories in `~/.config/quota-cli/config.toml` (or under `$XDG_CONFIG_HOME`, or in the file `QUOTA_CLI_CONFIG` names), each with a label for the report:

```toml
[[claude]]
label = "work"
config_dir = "~/.claude-work"

[[codex]]
label = "client"
home = "~/.codex-client"
```

`usage` reports the login each CLI is using now, plus every login in the file. When the current login is the same account as one in the file, it shows once, under the file's label. Claude logins are matched by the account Claude Code records in `.claude.json`, and Codex logins by the workspace and email in the token. A login in the file that cannot be read always shows, with the reason, so a typo in the file does not go unnoticed. An unknown key in the file is an error for the same reason.

Write `config_dir` the way you set `CLAUDE_CONFIG_DIR`. On macOS, Claude Code names the Keychain item after that exact text, so `~/.claude-work` and `~/.claude-work/` are two different items. Naming the default `~/.claude` means the default login.

The Herdr plugin keeps reporting the login each CLI is using now. A pane does not say which config directory its agent was started with.

## Limit resets

Codex and Claude can each bank one-time resets that refill a usage window when you choose, instead of when it rolls over. `usage` shows how many an account has, and when the next one expires if the provider says.

- Codex reports the count in the same usage call, as `rate_limit_reset_credits`. Its expiry dates need a separate call, which `quota-cli` does not make.
- Claude reports resets per grant, with an end date, in the usage call when it asks for them (`cedar_ember=1`). The API only answers that part for a client whose user agent starts with `claude-cli/`, so the Claude usage call sends `claude-cli/<version> (external, cli) quota/<version>`. Any other user agent gets `ineligible_reason: "surface"`, which `quota-cli` treats as unknown, not as zero.

`quota-cli` only reads these counts. Using a reset stays with the provider's own CLI.

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
| Claude | `.credentials.json` in `CLAUDE_CONFIG_DIR` (default `~/.claude`), or the macOS Keychain when that file is missing |
| Codex | `~/.codex/auth.json`, honoring `CODEX_HOME` |
| Cursor | `~/.config/cursor/auth.json` |
| Antigravity | OS keyring, falling back to `~/.gemini/oauth_creds.json` |
| Grok | `~/.grok/auth.json`, honoring `GROK_HOME` |
| Kiro | `~/.local/share/kiro-cli/data.sqlite3`, falling back to the AWS SSO cache |

Three of these are worth knowing about. Cursor, Kiro, and Antigravity keep separate credentials for their CLI and their IDE, and the two stores can hold different accounts or lapse independently. `quota-cli` reads the CLI store first, because that is the session an agent in a terminal actually uses.

## What it does not do

Every reader is strictly read-only, and so is the config file. Nothing is refreshed, rewritten, or rotated, because those tokens back your own agent sessions and rotating one could leave your real CLI holding a dead credential. Antigravity is the single exception, and only because Google does not rotate its refresh token: a fresh access token is held in memory for one request and nothing is written to disk.

The practical effect shows up with Kiro. Its CLI refreshes its token only when it has a reason to call the service, so an idle machine holds a lapsed token and `quota-cli` reports that instead of a number. Using Kiro refreshes it.

Tokens are never logged or printed. Each credential struct has a hand-written `Debug` that redacts, and parse failures report only a line and column, because serde's own error message can quote the input it failed on.

## License

Apache-2.0. Made with 💖 by Pink Pixel.

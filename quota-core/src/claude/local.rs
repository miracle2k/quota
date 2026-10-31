//! Access to the credentials the Claude Code CLI stores locally.
//!
//! Everything here only reads. Renewing an expired token lives in
//! [`super::refresh`], which follows Claude Code's own locking so the two never
//! fight over the refresh token.

use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::fmt;
use std::path::{Path, PathBuf};

/// Where the Claude Code CLI writes its credentials. `CLAUDE_CONFIG_DIR`
/// relocates it.
pub fn local_credentials_path() -> Option<PathBuf> {
    ClaudeLogin::current().map(|login| login.credentials_path())
}

/// One place Claude Code keeps a login: a config directory, plus the Keychain
/// item it uses for that directory on macOS.
///
/// Claude Code treats the default directory and one chosen through
/// `CLAUDE_CONFIG_DIR` differently. The default login lives in the plain
/// Keychain item and records its account in `~/.claude.json`. A login made with
/// `CLAUDE_CONFIG_DIR` gets a Keychain item named after a hash of that variable's
/// value, and records its account inside the directory itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeLogin {
    config_dir: PathBuf,
    /// The `CLAUDE_CONFIG_DIR` value, or `None` for the default directory.
    config_dir_override: Option<String>,
}

impl ClaudeLogin {
    /// The login Claude Code would use in this environment.
    pub fn current() -> Option<Self> {
        match std::env::var("CLAUDE_CONFIG_DIR") {
            Ok(value) if !value.is_empty() => Some(Self::for_config_dir(&value)),
            _ => Self::default_dir(),
        }
    }

    /// The login in `~/.claude`, used when `CLAUDE_CONFIG_DIR` is not set.
    pub fn default_dir() -> Option<Self> {
        dirs::home_dir().map(|home| Self {
            config_dir: home.join(".claude"),
            config_dir_override: None,
        })
    }

    /// The login Claude Code keeps when started with
    /// `CLAUDE_CONFIG_DIR=<config_dir>`. Pass the value exactly as it is set
    /// there: Claude Code names the Keychain item after that text, so a
    /// trailing slash or an unexpanded `~` names a different item.
    pub fn for_config_dir(config_dir: &str) -> Self {
        Self {
            config_dir: PathBuf::from(config_dir),
            config_dir_override: Some(config_dir.to_string()),
        }
    }

    pub fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    pub fn credentials_path(&self) -> PathBuf {
        self.config_dir.join(".credentials.json")
    }

    /// The Keychain service Claude Code stores this login under on macOS.
    pub fn keychain_service(&self) -> String {
        match &self.config_dir_override {
            Some(config_dir) => keychain_service_for_config_dir(config_dir),
            None => KEYCHAIN_SERVICE.to_string(),
        }
    }

    /// The `.claude.json` file where Claude Code records the signed-in account.
    pub fn account_file_path(&self) -> Option<PathBuf> {
        match &self.config_dir_override {
            Some(_) => Some(self.config_dir.join(".claude.json")),
            None => dirs::home_dir().map(|home| home.join(".claude.json")),
        }
    }

    /// Read this login from wherever Claude Code keeps it.
    pub fn read_credentials(&self) -> Result<LocalClaudeCredentials, LocalCredentialError> {
        let (_, raw) = self.read_stored()?;
        parse_credentials_json(raw.trim())
    }

    /// The stored credentials JSON, and the store it came from so a renewed
    /// token can go back to the same place. On macOS the Keychain item comes
    /// first and the file is only a fallback, which is the order Claude Code
    /// reads them in: it writes the file only when the Keychain refuses.
    pub fn read_stored(&self) -> Result<(CredentialStore, String), LocalCredentialError> {
        #[cfg(target_os = "macos")]
        let keychain_error = {
            let service = self.keychain_service();
            let account = keychain_account();
            match read_keychain_item(&service, account.as_deref()) {
                Ok(raw) => return Ok((CredentialStore::Keychain { service, account }, raw)),
                Err(LocalCredentialError::NotFound) => None,
                Err(err) => Some(err),
            }
        };

        let path = self.credentials_path();
        match std::fs::read_to_string(&path) {
            Ok(raw) => Ok((CredentialStore::File(path), raw)),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                // A Keychain that could not be read says more than a file
                // that was never expected to exist.
                #[cfg(target_os = "macos")]
                if let Some(err) = keychain_error {
                    return Err(err);
                }
                Err(LocalCredentialError::NotFound)
            }
            Err(err) => Err(LocalCredentialError::Unreadable(err.to_string())),
        }
    }

    /// Which account this login is signed in as, so two directories holding
    /// the same account can be told apart from two different accounts. `None`
    /// when Claude Code has not recorded it.
    pub fn account_identity(&self) -> Option<String> {
        parse_account_identity(&self.read_account_file()?)
    }

    /// The signed-in account as Claude Code recorded it in `.claude.json`.
    pub fn account_info(&self) -> Option<AccountInfo> {
        parse_account_info(&self.read_account_file()?)
    }

    fn read_account_file(&self) -> Option<String> {
        std::fs::read_to_string(self.account_file_path()?).ok()
    }
}

/// Where one login's credentials live.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialStore {
    /// `.credentials.json` inside the config directory.
    File(PathBuf),
    /// A macOS Keychain item. `account` is the item's account name, `None`
    /// when it was found without one.
    Keychain {
        service: String,
        account: Option<String>,
    },
}

impl CredentialStore {
    /// Read this store and no other. Unlike [`ClaudeLogin::read_stored`],
    /// nothing falls back to a different store, so a compare-and-swap checks
    /// the store it is about to write.
    pub fn read(&self) -> Result<String, LocalCredentialError> {
        match self {
            Self::File(path) => std::fs::read_to_string(path).map_err(|err| {
                if err.kind() == std::io::ErrorKind::NotFound {
                    LocalCredentialError::NotFound
                } else {
                    LocalCredentialError::Unreadable(err.to_string())
                }
            }),
            #[cfg(target_os = "macos")]
            Self::Keychain { service, account } => read_keychain_item(service, account.as_deref()),
            #[cfg(not(target_os = "macos"))]
            Self::Keychain { .. } => Err(LocalCredentialError::Unreadable(
                "no Keychain on this system".to_string(),
            )),
        }
    }

    /// Whether a secret of `secret_len` bytes can be written here without it
    /// appearing in a command line. Only the Keychain has a limit:
    /// `security -i` reads at most 4032 bytes per command, and the secret
    /// goes in hex.
    pub fn fits(&self, secret_len: usize) -> bool {
        match self {
            Self::File(_) => true,
            Self::Keychain { service, account } => {
                keychain_command_len(
                    service,
                    account.as_deref().unwrap_or("claude-code-user"),
                    secret_len,
                ) <= KEYCHAIN_COMMAND_LIMIT
            }
        }
    }
}

/// The longest command `security -i` reads from stdin.
const KEYCHAIN_COMMAND_LIMIT: usize = 4032;

/// The length of the `add-generic-password` command line that stores a
/// secret of `secret_len` bytes.
fn keychain_command_len(service: &str, account: &str, secret_len: usize) -> usize {
    "add-generic-password -U -a \"\" -s \"\" -X \"\"\n".len()
        + account.len()
        + service.len()
        + secret_len * 2
}

/// The Keychain service Claude Code uses for a login made with
/// `CLAUDE_CONFIG_DIR`: the plain service name plus the first eight hex digits
/// of the SHA-256 of the variable's value.
///
/// Claude Code normalizes the value to Unicode NFC before hashing. This does
/// not, which only matters for a directory path with non-ASCII characters.
pub fn keychain_service_for_config_dir(config_dir: &str) -> String {
    let digest = Sha256::digest(config_dir.as_bytes());
    let prefix: String = digest
        .iter()
        .take(4)
        .map(|byte| format!("{:02x}", byte))
        .collect();
    format!("{}-{}", KEYCHAIN_SERVICE, prefix)
}

/// Pull the signed-in account out of a `.claude.json` file as
/// `<organization uuid>:<account uuid>`. The organization is part of it because
/// one person can sign in to a personal and a team organization, and each has
/// its own limits.
pub fn parse_account_identity(raw: &str) -> Option<String> {
    #[derive(Deserialize)]
    struct AccountFile {
        #[serde(rename = "oauthAccount")]
        oauth_account: Option<OauthAccount>,
    }

    #[derive(Deserialize)]
    struct OauthAccount {
        #[serde(rename = "accountUuid")]
        account_uuid: Option<String>,
        #[serde(rename = "organizationUuid")]
        organization_uuid: Option<String>,
    }

    let account = serde_json::from_str::<AccountFile>(raw)
        .ok()?
        .oauth_account?;
    let account_uuid = account.account_uuid.filter(|uuid| !uuid.is_empty())?;
    Some(format!(
        "{}:{}",
        account.organization_uuid.unwrap_or_default(),
        account_uuid
    ))
}

/// The account a login is signed in to, from the `oauthAccount` block of a
/// `.claude.json` file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AccountInfo {
    pub account_uuid: Option<String>,
    /// Empty for a personal account in some tools; kept as found.
    pub organization_uuid: Option<String>,
    pub email: Option<String>,
}

/// Pull the signed-in account out of a `.claude.json` file. `None` when the
/// file records no account.
pub fn parse_account_info(raw: &str) -> Option<AccountInfo> {
    let parsed: serde_json::Value = serde_json::from_str(raw).ok()?;
    let account = parsed.get("oauthAccount")?;
    let text = |key: &str| {
        account
            .get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    let info = AccountInfo {
        account_uuid: text("accountUuid"),
        organization_uuid: text("organizationUuid"),
        email: text("emailAddress"),
    };
    (info.account_uuid.is_some() || info.email.is_some()).then_some(info)
}

#[derive(Clone)]
pub struct LocalClaudeCredentials {
    pub access_token: String,
    pub expires_at_ms: Option<i64>,
    pub scopes: Vec<String>,
    pub subscription_type: Option<String>,
    pub organization_uuid: Option<String>,
}

impl LocalClaudeCredentials {
    /// True when the stored expiry is at or before `now_ms`. Unknown expiry is
    /// treated as usable, because the endpoint is the real authority.
    pub fn is_expired_at_ms(&self, now_ms: i64) -> bool {
        match self.expires_at_ms {
            Some(expiry) => expiry <= now_ms,
            None => false,
        }
    }
}

/// Deliberately hand-written so a token can never reach the Debug output.
impl fmt::Debug for LocalClaudeCredentials {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LocalClaudeCredentials")
            .field("access_token", &"<redacted>")
            .field("expires_at_ms", &self.expires_at_ms)
            .field("scopes", &self.scopes)
            .field("subscription_type", &self.subscription_type)
            .field("organization_uuid", &self.organization_uuid)
            .finish()
    }
}

#[derive(Debug)]
pub enum LocalCredentialError {
    NotFound,
    Unreadable(String),
    Malformed(String),
}

impl fmt::Display for LocalCredentialError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound => write!(
                formatter,
                "No Claude Code credentials found. Sign in with the Claude Code CLI first."
            ),
            Self::Unreadable(detail) => {
                write!(formatter, "Could not read Claude Code credentials: {}", detail)
            }
            Self::Malformed(detail) => write!(
                formatter,
                "Claude Code credentials are not in the expected shape: {}",
                detail
            ),
        }
    }
}

#[derive(Deserialize)]
struct CredentialsFile {
    #[serde(rename = "claudeAiOauth")]
    oauth: Option<OauthBlock>,
    #[serde(rename = "organizationUuid")]
    organization_uuid: Option<String>,
}

#[derive(Deserialize)]
struct OauthBlock {
    #[serde(rename = "accessToken")]
    access_token: Option<String>,
    #[serde(rename = "expiresAt")]
    expires_at: Option<i64>,
    #[serde(default)]
    scopes: Vec<String>,
    #[serde(rename = "subscriptionType")]
    subscription_type: Option<String>,
}

pub fn read_local_credentials_at(path: &Path) -> Result<LocalClaudeCredentials, LocalCredentialError> {
    if !path.exists() {
        return Err(LocalCredentialError::NotFound);
    }

    let raw = std::fs::read_to_string(path)
        .map_err(|err| LocalCredentialError::Unreadable(err.to_string()))?;

    parse_credentials_json(&raw)
}

/// Parse the credentials JSON. The file and the macOS Keychain item hold the
/// same payload, so both sources go through here.
pub fn parse_credentials_json(raw: &str) -> Result<LocalClaudeCredentials, LocalCredentialError> {
    let parsed: CredentialsFile = serde_json::from_str(raw)
        .map_err(|err| {
            // Use only line/column info from serde error, never the quoted value which could be a token
            let msg = format!(
                "JSON error at line {} column {}",
                err.line(),
                err.column()
            );
            LocalCredentialError::Malformed(msg)
        })?;

    let oauth = parsed
        .oauth
        .ok_or_else(|| LocalCredentialError::Malformed("no claudeAiOauth block".to_string()))?;

    let access_token = oauth
        .access_token
        .filter(|token| !token.is_empty())
        .ok_or_else(|| LocalCredentialError::Malformed("no access token".to_string()))?;

    Ok(LocalClaudeCredentials {
        access_token,
        expires_at_ms: oauth.expires_at,
        scopes: oauth.scopes,
        subscription_type: oauth.subscription_type,
        organization_uuid: parsed.organization_uuid,
    })
}

/// The Keychain service Claude Code stores its login under on macOS, where it
/// does not write the credentials file at all.
pub const KEYCHAIN_SERVICE: &str = "Claude Code-credentials";

/// Read the login Claude Code would use in this environment, falling back to
/// the Keychain on macOS when the file is missing.
pub fn read_local_credentials() -> Result<LocalClaudeCredentials, LocalCredentialError> {
    ClaudeLogin::current()
        .ok_or_else(|| LocalCredentialError::Unreadable("no home directory".to_string()))?
        .read_credentials()
}

/// The account name Claude Code files its Keychain items under: the login
/// name, or a fixed stand-in when that has characters it does not accept.
/// `None` when no login name is set, in which case any account matches.
#[cfg(target_os = "macos")]
fn keychain_account() -> Option<String> {
    let user = std::env::var("USER").ok().filter(|user| !user.is_empty())?;
    let usable = user
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    Some(if usable {
        user
    } else {
        "claude-code-user".to_string()
    })
}

/// Read a Keychain item through `/usr/bin/security`. Claude Code writes the
/// item with that same tool, so its access list already allows this read and
/// no unlock prompt appears.
#[cfg(target_os = "macos")]
fn read_keychain_item(
    service: &str,
    account: Option<&str>,
) -> Result<String, LocalCredentialError> {
    // `security` exits with 44 when no item matches.
    const ITEM_NOT_FOUND: i32 = 44;

    let mut command = std::process::Command::new("/usr/bin/security");
    command.arg("find-generic-password");
    if let Some(account) = account {
        command.args(["-a", account]);
    }
    let output = command
        .args(["-s", service, "-w"])
        .output()
        .map_err(|err| LocalCredentialError::Unreadable(err.to_string()))?;

    if !output.status.success() {
        return Err(match output.status.code() {
            Some(ITEM_NOT_FOUND) => LocalCredentialError::NotFound,
            _ => {
                LocalCredentialError::Unreadable(format!("security exited with {}", output.status))
            }
        });
    }

    String::from_utf8(output.stdout)
        .map(|raw| raw.trim().to_string())
        .map_err(|_| LocalCredentialError::Malformed("Keychain item is not UTF-8".to_string()))
}

/// Replace a Keychain item's secret the way Claude Code does: through
/// `security -i`, so the secret travels on stdin instead of in an argument
/// list other processes can read. Updating with the same tool that created the
/// item keeps its access list unchanged. A secret too long for one stdin
/// command is refused; Claude Code would put it on the command line instead.
#[cfg(target_os = "macos")]
pub(crate) fn write_keychain_item(
    service: &str,
    account: Option<&str>,
    secret: &str,
) -> Result<(), String> {
    use std::io::Write;

    let account = match account {
        Some(account) => account.to_string(),
        None => keychain_item_account(service)
            .ok_or_else(|| "could not tell which Keychain account holds the login".to_string())?,
    };
    if service.contains('"') || account.contains('"') {
        return Err("unexpected quote in Keychain item name".to_string());
    }
    if keychain_command_len(service, &account, secret.len()) > KEYCHAIN_COMMAND_LIMIT {
        return Err(
            "the login is too long to hand to `security` without a command line".to_string(),
        );
    }
    let hex: String = secret.bytes().map(|byte| format!("{:02x}", byte)).collect();
    let line = format!(
        "add-generic-password -U -a \"{}\" -s \"{}\" -X \"{}\"\n",
        account, service, hex
    );

    let mut child = std::process::Command::new("/usr/bin/security")
        .arg("-i")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|err| err.to_string())?;
    let written = child
        .stdin
        .take()
        .map(|mut stdin| stdin.write_all(line.as_bytes()))
        .unwrap_or(Ok(()));
    let status = child.wait().map_err(|err| err.to_string())?;
    written.map_err(|err| err.to_string())?;

    if status.success() {
        Ok(())
    } else {
        Err(format!("security exited with {}", status))
    }
}

/// The account name of the Keychain item filed under `service`, read from the
/// item's attributes. The secret itself is not requested.
#[cfg(target_os = "macos")]
fn keychain_item_account(service: &str) -> Option<String> {
    let output = std::process::Command::new("/usr/bin/security")
        .args(["find-generic-password", "-s", service])
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_keychain_account_attribute(&String::from_utf8_lossy(&output.stdout))
}

/// Find the `"acct"<blob>="..."` line in `security find-generic-password`
/// output.
pub fn parse_keychain_account_attribute(attributes: &str) -> Option<String> {
    attributes.lines().find_map(|line| {
        let value = line.trim().strip_prefix("\"acct\"<blob>=\"")?;
        let value = value.strip_suffix('"')?;
        (!value.is_empty()).then(|| value.to_string())
    })
}

//! Z.ai (GLM) usage for an API key: the GLM Coding Plan's windows when the
//! account has a plan, and the prepaid balance either way.
//!
//! Z.ai runs the same service in two regions: `api.z.ai` bills in US dollars,
//! and BigModel (`open.bigmodel.cn`) in yuan. Both answer the same calls.

use crate::usage::{Balance, ProviderUsage, UsageWindow};
use serde_json::Value;

/// The environment variable pi and other tools read the key from.
pub const API_KEY_ENV: &str = "ZAI_API_KEY";

/// What the quota endpoint says when the account has no Coding Plan.
const NO_CODING_PLAN: &str = "当前用户不存在coding plan";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Region {
    /// `api.z.ai`, billed in US dollars.
    Global,
    /// BigModel, `open.bigmodel.cn`, billed in yuan.
    China,
}

impl Region {
    fn quota_url(self) -> &'static str {
        match self {
            Self::Global => "https://api.z.ai/api/monitor/usage/quota/limit",
            Self::China => "https://open.bigmodel.cn/api/monitor/usage/quota/limit",
        }
    }

    fn balance_url(self) -> &'static str {
        match self {
            Self::Global => "https://api.z.ai/api/biz/account/query-customer-account-report",
            Self::China => "https://www.bigmodel.cn/api/biz/account/query-customer-account-report",
        }
    }

    fn currency(self) -> &'static str {
        match self {
            Self::Global => "USD",
            Self::China => "CNY",
        }
    }
}

#[derive(Debug)]
pub enum ZaiError {
    /// No API key here.
    NotConfigured,
    /// The key is set but cannot be used, such as a pi key behind a command.
    Key(String),
    Unauthorized,
    /// No Coding Plan, and no balance either.
    NoPlan,
    Request(String),
}

impl ZaiError {
    pub fn is_not_configured(&self) -> bool {
        matches!(self, Self::NotConfigured)
    }
}

impl std::fmt::Display for ZaiError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotConfigured => write!(
                formatter,
                "No Z.ai API key found. Set {} or sign in to Z.ai in pi.",
                API_KEY_ENV
            ),
            Self::Key(detail) => write!(formatter, "{}", detail),
            Self::Unauthorized => write!(formatter, "Z.ai rejected the API key."),
            Self::NoPlan => write!(
                formatter,
                "This Z.ai account has no GLM Coding Plan and reported no balance."
            ),
            Self::Request(detail) => write!(formatter, "Z.ai usage request failed: {}", detail),
        }
    }
}

/// What the quota endpoint reported.
#[derive(Debug, Clone, PartialEq)]
pub enum Quota {
    Plan {
        name: Option<String>,
        windows: Vec<UsageWindow>,
    },
    NoPlan,
}

/// Read the quota endpoint's answer.
///
/// Each limit has a `type`: `TOKENS_LIMIT` (or `CREDIT_LIMIT`) for the prompt
/// windows, shortest first, and `TIME_LIMIT` for the monthly allowance of web
/// search and reader calls. `unit` and `number` give the window length (unit 3
/// is hours, 6 weeks, 1 days); `percentage` is the share used.
pub fn parse_quota(raw: &Value) -> Result<Quota, ZaiError> {
    if raw.get("success").and_then(Value::as_bool) != Some(true) {
        if raw.get("msg").and_then(Value::as_str) == Some(NO_CODING_PLAN) {
            return Ok(Quota::NoPlan);
        }
        let message = raw
            .get("msg")
            .and_then(Value::as_str)
            .unwrap_or("unexpected answer");
        return Err(ZaiError::Request(message.to_string()));
    }
    let data = raw.get("data").unwrap_or(&Value::Null);
    let limits = data
        .get("limits")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let mut prompts: Vec<(i64, UsageWindow)> = Vec::new();
    let mut tools: Vec<UsageWindow> = Vec::new();
    for limit in &limits {
        let kind = limit
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let minutes = window_minutes(limit);
        let window = UsageWindow {
            label: String::new(),
            remaining_percent: used_percent(limit).map(|used| (100.0 - used).round() as i32),
            reset_at: limit
                .get("nextResetTime")
                .and_then(Value::as_i64)
                .map(|ms| ms / 1000),
        };
        match kind {
            "TOKENS_LIMIT" | "CREDIT_LIMIT" => {
                let label = minutes
                    .map(label_for_minutes)
                    .unwrap_or_else(|| "Plan".to_string());
                prompts.push((minutes.unwrap_or(i64::MAX), UsageWindow { label, ..window }));
            }
            "TIME_LIMIT" => tools.push(UsageWindow {
                label: "MCP".to_string(),
                ..window
            }),
            _ => {}
        }
    }
    prompts.sort_by_key(|(minutes, _)| *minutes);

    let name = ["planName", "plan", "plan_type", "packageName", "level"]
        .iter()
        .filter_map(|key| data.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .find(|name| !name.is_empty())
        .map(str::to_string);
    Ok(Quota::Plan {
        name,
        windows: prompts
            .into_iter()
            .map(|(_, window)| window)
            .chain(tools)
            .collect(),
    })
}

/// The share used, in percent. The counts win over the rounded percentage
/// when the limit carries them.
fn used_percent(limit: &Value) -> Option<f64> {
    let total = limit
        .get("usage")
        .and_then(Value::as_f64)
        .filter(|total| *total > 0.0);
    let used = limit
        .get("currentValue")
        .and_then(Value::as_f64)
        .or_else(|| {
            let remaining = limit.get("remaining").and_then(Value::as_f64)?;
            Some(total? - remaining)
        });
    let percent = match (total, used) {
        (Some(total), Some(used)) => used / total * 100.0,
        _ => limit.get("percentage").and_then(Value::as_f64)?,
    };
    Some(percent.clamp(0.0, 100.0))
}

fn window_minutes(limit: &Value) -> Option<i64> {
    let unit = limit.get("unit").and_then(Value::as_i64)?;
    let number = limit.get("number").and_then(Value::as_i64)?;
    // Unit 5 with number 1 marks a monthly allowance.
    if unit == 5 && number == 1 {
        return Some(30 * 1440);
    }
    let unit_minutes = match unit {
        1 => 1440,
        3 => 60,
        5 => 1,
        6 => 10080,
        _ => return None,
    };
    Some(number * unit_minutes)
}

fn label_for_minutes(minutes: i64) -> String {
    match minutes {
        10080 => "Wk".to_string(),
        43200 => "Mo".to_string(),
        m if m % 1440 == 0 => format!("{}d", m / 1440),
        m if m % 60 == 0 => format!("{}h", m / 60),
        m => format!("{}m", m),
    }
}

/// Read the balance endpoint's answer.
pub fn parse_balance(raw: &Value, region: Region) -> Option<Balance> {
    if raw.get("success").and_then(Value::as_bool) != Some(true) {
        return None;
    }
    let data = raw.get("data")?;
    let amount = data
        .get("availableBalance")
        .and_then(Value::as_f64)
        .or_else(|| data.get("balance").and_then(Value::as_f64))?;
    Some(Balance {
        amount,
        currency: region.currency().to_string(),
    })
}

/// Fetch the plan's windows and the balance for one API key.
pub async fn fetch_usage(key: &str, region: Region) -> Result<ProviderUsage, ZaiError> {
    let quota = parse_quota(&get_json(region.quota_url(), key).await?)?;
    // The balance is a detail when there is a plan. A failure here never hides
    // the plan's numbers.
    let balance = get_json(region.balance_url(), key)
        .await
        .ok()
        .and_then(|raw| parse_balance(&raw, region));

    match quota {
        Quota::Plan { name, windows } => Ok(ProviderUsage {
            provider: "zai".to_string(),
            account_label: name,
            windows,
            note: None,
            resets: None,
            balance,
        }),
        Quota::NoPlan if balance.is_some() => Ok(ProviderUsage {
            provider: "zai".to_string(),
            account_label: None,
            windows: Vec::new(),
            note: Some("No GLM Coding Plan".to_string()),
            resets: None,
            balance,
        }),
        Quota::NoPlan => Err(ZaiError::NoPlan),
    }
}

async fn get_json(url: &str, key: &str) -> Result<Value, ZaiError> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|err| ZaiError::Request(err.to_string()))?;
    let response = client
        .get(url)
        .header(reqwest::header::AUTHORIZATION, format!("Bearer {}", key))
        .header(reqwest::header::ACCEPT, "application/json")
        .header(reqwest::header::USER_AGENT, "quota")
        .send()
        .await
        .map_err(|err| ZaiError::Request(crate::http_error::describe(err)))?;
    let status = response.status().as_u16();
    if status == 401 {
        return Err(ZaiError::Unauthorized);
    }
    if !(200..300).contains(&status) {
        return Err(ZaiError::Request(format!("status={}", status)));
    }
    response
        .json()
        .await
        .map_err(|err| ZaiError::Request(crate::http_error::describe(err)))
}

/// The key this environment would use, with its region: `ZAI_API_KEY`, then
/// pi's `zai` login, then pi's BigModel login.
pub fn local_key() -> Result<(String, Region), ZaiError> {
    if let Ok(key) = std::env::var(API_KEY_ENV) {
        if !key.trim().is_empty() {
            return Ok((key.trim().to_string(), Region::Global));
        }
    }
    let auth = crate::pi::read_auth().map_err(|err| ZaiError::Key(err.to_string()))?;
    for (provider, region) in PI_PROVIDERS {
        match crate::pi::login(&auth, provider) {
            Ok(crate::pi::PiCredential::ApiKey(key)) => return Ok((key, *region)),
            Err(crate::pi::PiError::NotFound) => continue,
            Ok(crate::pi::PiCredential::OAuth(_)) => continue,
            Err(err) => return Err(ZaiError::Key(err.to_string())),
        }
    }
    Err(ZaiError::NotConfigured)
}

/// pi's names for the two regions.
pub const PI_PROVIDERS: &[(&str, Region)] =
    &[("zai", Region::Global), ("zai-coding-cn", Region::China)];

/// Fetch usage for the key this environment would use.
pub async fn fetch_local_usage() -> Result<ProviderUsage, ZaiError> {
    let (key, region) = local_key()?;
    fetch_usage(&key, region).await
}

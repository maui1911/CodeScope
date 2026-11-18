//! Claude plan-limit usage (5-hour session, 7-day week, monthly extra
//! usage) for the status bar.
//!
//! Source is the same endpoint the Claude CLI's own `/usage` view uses:
//! `GET https://api.anthropic.com/api/oauth/usage`, authenticated with the
//! OAuth access token the CLI keeps in `~/.claude/.credentials.json`. The
//! endpoint is undocumented, so every parse step is tolerant — a missing
//! or reshaped field yields `None` for that window, never an error the UI
//! has to render. We only *read* the token the CLI already refreshes; we
//! never refresh or write it, and it is sent to `api.anthropic.com` only.
//!
//! The network half is blocking and meant for a background thread, like
//! `update_check`. The parse half is pure and unit-tested.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::Value;

const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// One rate-limit window: how full it is and when it resets.
#[derive(Debug, Clone, PartialEq)]
pub struct UsageWindow {
    /// `0.0..=100.0` (the API may report slightly above 100 when over).
    pub percent: f32,
    /// RFC 3339 reset time as the API sent it; `None` when absent.
    pub resets_at: Option<String>,
}

/// Plan usage snapshot. Each window is `None` when the plan doesn't have
/// it or the API omitted it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PlanUsage {
    /// Rolling 5-hour session window.
    pub five_hour: Option<UsageWindow>,
    /// Rolling 7-day window across all models.
    pub seven_day: Option<UsageWindow>,
    /// Monthly extra-usage (overage credits) spend, only when the user
    /// has enabled it and set a monthly limit.
    pub month: Option<UsageWindow>,
}

/// Parse the `/api/oauth/usage` response body. `None` when the body isn't
/// a JSON object.
pub fn parse_usage(body: &str) -> Option<PlanUsage> {
    let root: Value = serde_json::from_str(body).ok()?;
    let root = root.as_object()?;
    let window = |key: &str| root.get(key).and_then(parse_window);

    let month = root
        .get("extra_usage")
        .filter(|e| e.get("is_enabled").and_then(Value::as_bool) == Some(true))
        .and_then(parse_window);

    Some(PlanUsage {
        five_hour: window("five_hour"),
        seven_day: window("seven_day"),
        month,
    })
}

fn parse_window(v: &Value) -> Option<UsageWindow> {
    let percent = v.get("utilization")?.as_f64()? as f32;
    let resets_at = v
        .get("resets_at")
        .and_then(Value::as_str)
        .map(str::to_owned);
    Some(UsageWindow { percent, resets_at })
}

/// Extract the OAuth access token from a `.credentials.json` body.
/// `None` when absent or when `expiresAt` (epoch ms) is at or before
/// `now_ms` — an expired token only earns a 401, and the CLI refreshes
/// it on its own next run.
pub fn access_token_from_credentials(body: &str, now_ms: u64) -> Option<String> {
    let root: Value = serde_json::from_str(body).ok()?;
    let oauth = root.get("claudeAiOauth")?;
    let token = oauth.get("accessToken")?.as_str()?;
    if token.is_empty() {
        return None;
    }
    if let Some(exp) = oauth.get("expiresAt").and_then(Value::as_u64)
        && exp <= now_ms
    {
        return None;
    }
    Some(token.to_owned())
}

/// `~/.claude/.credentials.json`, honouring `CLAUDE_CONFIG_DIR` like the
/// CLI does.
pub fn credentials_path() -> Option<PathBuf> {
    let dir = match std::env::var_os("CLAUDE_CONFIG_DIR") {
        Some(d) => PathBuf::from(d),
        None => {
            // Same order as `claude::telemetry::default_projects_root`:
            // on Windows a Git Bash `HOME` can point somewhere else.
            let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
            PathBuf::from(home).join(".claude")
        }
    };
    Some(dir.join(".credentials.json"))
}

/// Fetch current plan usage. Blocking; call from a background thread.
///
/// Errors are for the caller to log and swallow: no credentials (API-key
/// users, not logged in), expired token, offline, or an API reshape all
/// just mean "no usage segment".
pub fn fetch_plan_usage() -> Result<PlanUsage> {
    let path = credentials_path().context("no home directory")?;
    fetch_with_credentials(&path)
}

fn fetch_with_credentials(path: &Path) -> Result<PlanUsage> {
    let body = std::fs::read_to_string(path).context("read credentials")?;
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64);
    let token = access_token_from_credentials(&body, now_ms)
        .context("no valid OAuth access token (expired or not logged in)")?;

    let client = reqwest::blocking::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()
        .context("build http client")?;
    let resp = client
        .get(USAGE_URL)
        .bearer_auth(token)
        .header("anthropic-beta", "oauth-2025-04-20")
        .header("User-Agent", concat!("codescope/", env!("CARGO_PKG_VERSION")))
        .send()
        .context("usage request")?;
    let status = resp.status();
    if !status.is_success() {
        bail!("usage endpoint returned {status}");
    }
    let text = resp.text().context("read usage body")?;
    parse_usage(&text).context("unrecognised usage response")
}

/// Compact label for a window: `"12%"`, `"3.5%"`, `"104%"`.
pub fn format_window_pct(percent: f32) -> String {
    let p = percent.max(0.0);
    if p >= 10.0 {
        format!("{:.0}%", p.round())
    } else {
        let one_dp = (p * 10.0).round() / 10.0;
        if (one_dp - one_dp.trunc()).abs() < f32::EPSILON {
            format!("{:.0}%", one_dp)
        } else {
            format!("{one_dp:.1}%")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
        "five_hour": {"utilization": 2.0, "resets_at": "2026-10-01T12:20:00+00:00"},
        "seven_day": {"utilization": 29.0, "resets_at": "2026-10-05T11:00:00+00:00"},
        "seven_day_opus": null,
        "extra_usage": {"is_enabled": false, "monthly_limit": null, "used_credits": null, "utilization": null}
    }"#;

    #[test]
    fn parses_five_hour_and_seven_day() {
        let u = parse_usage(SAMPLE).unwrap();
        assert_eq!(
            u.five_hour,
            Some(UsageWindow {
                percent: 2.0,
                resets_at: Some("2026-10-01T12:20:00+00:00".into())
            })
        );
        assert_eq!(u.seven_day.unwrap().percent, 29.0);
    }

    #[test]
    fn disabled_extra_usage_has_no_month() {
        assert_eq!(parse_usage(SAMPLE).unwrap().month, None);
    }

    #[test]
    fn enabled_extra_usage_becomes_month() {
        let body = r#"{"extra_usage": {"is_enabled": true, "monthly_limit": 5000,
            "used_credits": 1250, "utilization": 25.0}}"#;
        let m = parse_usage(body).unwrap().month.unwrap();
        assert_eq!(m.percent, 25.0);
        assert_eq!(m.resets_at, None);
    }

    #[test]
    fn enabled_extra_usage_without_utilization_has_no_month() {
        let body = r#"{"extra_usage": {"is_enabled": true, "utilization": null}}"#;
        assert_eq!(parse_usage(body).unwrap().month, None);
    }

    #[test]
    fn null_or_missing_windows_are_none() {
        let u = parse_usage(r#"{"five_hour": null}"#).unwrap();
        assert_eq!(u, PlanUsage::default());
    }

    #[test]
    fn non_object_or_garbage_is_none() {
        assert_eq!(parse_usage("[]"), None);
        assert_eq!(parse_usage("not json"), None);
    }

    #[test]
    fn token_read_from_credentials() {
        let body = r#"{"claudeAiOauth": {"accessToken": "tok", "expiresAt": 2000}}"#;
        assert_eq!(access_token_from_credentials(body, 1000), Some("tok".into()));
    }

    #[test]
    fn expired_token_rejected() {
        let body = r#"{"claudeAiOauth": {"accessToken": "tok", "expiresAt": 1000}}"#;
        assert_eq!(access_token_from_credentials(body, 1000), None);
        assert_eq!(access_token_from_credentials(body, 5000), None);
    }

    #[test]
    fn token_without_expiry_accepted() {
        let body = r#"{"claudeAiOauth": {"accessToken": "tok"}}"#;
        assert_eq!(access_token_from_credentials(body, 1000), Some("tok".into()));
    }

    #[test]
    fn missing_or_empty_token_rejected() {
        assert_eq!(access_token_from_credentials("{}", 0), None);
        assert_eq!(
            access_token_from_credentials(r#"{"claudeAiOauth": {"accessToken": ""}}"#, 0),
            None
        );
        assert_eq!(access_token_from_credentials("nope", 0), None);
    }

    #[test]
    fn window_pct_formatting() {
        assert_eq!(format_window_pct(0.0), "0%");
        assert_eq!(format_window_pct(2.0), "2%");
        assert_eq!(format_window_pct(3.46), "3.5%");
        assert_eq!(format_window_pct(29.4), "29%");
        assert_eq!(format_window_pct(104.0), "104%");
        assert_eq!(format_window_pct(-1.0), "0%");
    }
}

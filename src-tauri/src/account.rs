//! Plan limits for the signed-in Claude account, read with the login Claude Code saves
//! (a file on Windows and Linux, the Keychain on macOS).
//! The endpoint is the one behind Claude Code's `/usage` command. It is undocumented and may change.
//! The token is never renewed here: renewing it would rotate the refresh token and log Claude Code out.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const LOGIN_EXPIRED: &str = "Login expired. Open Claude Code to renew it.";
const NOT_SIGNED_IN: &str = "Not signed in to Claude Code.";
const OFFLINE: &str = "Can't reach Anthropic. Check the connection.";

// The endpoint answers 429 when polled every minute.
pub const CHECK_EVERY: Duration = Duration::from_secs(5 * 60);
// For failures that never reached Anthropic (offline, expired or missing login).
const RETRY_SOON: Duration = Duration::from_secs(60);
const RATE_LIMIT_PAUSE: Duration = Duration::from_secs(15 * 60);

pub struct FetchError {
    pub message: String,
    pub retry_in: Duration,
}

fn fail(message: impl Into<String>, retry_in: Duration) -> FetchError {
    FetchError { message: message.into(), retry_in }
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Limit {
    pub kind: String,
    pub label: String,
    pub percent: f64,
    pub level: &'static str,
    pub resets_at: Option<String>,
}

#[derive(Serialize, Clone, Debug)]
pub struct Account {
    pub limits: Vec<Limit>,
    pub plan: Option<String>,
    pub checked_at: String,
}

#[derive(Deserialize)]
struct Credentials {
    #[serde(rename = "claudeAiOauth")]
    oauth: OAuth,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct OAuth {
    access_token: String,
    expires_at: u64,
    subscription_type: Option<String>,
    rate_limit_tier: Option<String>,
}

#[derive(Deserialize)]
struct UsageReply {
    limits: Vec<RawLimit>,
}

#[derive(Deserialize)]
struct RawLimit {
    kind: String,
    percent: f64,
    resets_at: Option<String>,
    scope: Option<Scope>,
}

#[derive(Deserialize)]
struct Scope {
    model: Option<ScopeModel>,
}

#[derive(Deserialize)]
struct ScopeModel {
    display_name: Option<String>,
}

/// Colour band shared by the tray icon and the window.
pub fn level(percent: f64) -> &'static str {
    match percent {
        p if p >= 80.0 => "high",
        p if p >= 50.0 => "warn",
        _ => "ok",
    }
}

pub fn fetch(claude_dirs: &[PathBuf]) -> Result<Account, FetchError> {
    let text = read_credentials(claude_dirs).ok_or_else(|| fail(NOT_SIGNED_IN, RETRY_SOON))?;
    let oauth = serde_json::from_str::<Credentials>(&text).map_err(|_| fail(NOT_SIGNED_IN, RETRY_SOON))?.oauth;
    let now_ms = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or_default();
    if oauth.expires_at <= now_ms {
        return Err(fail(LOGIN_EXPIRED, RETRY_SOON));
    }

    let body = ureq::get(USAGE_URL)
        .set("Authorization", &format!("Bearer {}", oauth.access_token))
        .set("anthropic-beta", "oauth-2025-04-20")
        .timeout(Duration::from_secs(15))
        .call()
        .map_err(|error| match error {
            ureq::Error::Status(401, _) => fail(LOGIN_EXPIRED, CHECK_EVERY),
            ureq::Error::Status(429, _) => fail("Anthropic is rate limiting this check. Trying again in 15 minutes.", RATE_LIMIT_PAUSE),
            ureq::Error::Status(code, _) => fail(format!("Anthropic returned error {code}."), CHECK_EVERY),
            ureq::Error::Transport(_) => fail(OFFLINE, RETRY_SOON),
        })?
        .into_string()
        .map_err(|_| fail(OFFLINE, RETRY_SOON))?;

    Ok(Account {
        limits: parse_limits(&body).map_err(|message| fail(message, CHECK_EVERY))?,
        plan: plan_label(&oauth),
        checked_at: chrono::Local::now().format("%H:%M").to_string(),
    })
}

fn read_credentials(claude_dirs: &[PathBuf]) -> Option<String> {
    claude_dirs
        .iter()
        .find_map(|dir| std::fs::read_to_string(dir.join(".credentials.json")).ok())
        .or_else(keychain_credentials)
}

/// Claude Code on macOS keeps its login in the Keychain instead of a file.
#[cfg(target_os = "macos")]
fn keychain_credentials() -> Option<String> {
    let output = std::process::Command::new("security")
        .args(["find-generic-password", "-s", "Claude Code-credentials", "-w"])
        .output()
        .ok()?;
    output.status.success().then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(not(target_os = "macos"))]
fn keychain_credentials() -> Option<String> {
    None
}

pub fn parse_limits(body: &str) -> Result<Vec<Limit>, String> {
    let reply: UsageReply = serde_json::from_str(body).map_err(|_| "Anthropic's reply changed shape.".to_string())?;
    Ok(reply
        .limits
        .into_iter()
        .map(|raw| Limit {
            label: label(&raw),
            level: level(raw.percent),
            percent: raw.percent,
            resets_at: raw.resets_at,
            kind: raw.kind,
        })
        .collect())
}

fn label(raw: &RawLimit) -> String {
    let model = raw.scope.as_ref().and_then(|s| s.model.as_ref()).and_then(|m| m.display_name.as_deref());
    match (raw.kind.as_str(), model) {
        ("session", _) => "Session".into(),
        ("weekly_all", _) => "Week".into(),
        (_, Some(model)) => format!("{model} week"),
        (kind, None) => kind.replace('_', " "),
    }
}

/// "team" + "default_claude_max_5x" -> "Team · Max 5x"
fn plan_label(oauth: &OAuth) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(kind) = &oauth.subscription_type {
        let mut chars = kind.chars();
        if let Some(first) = chars.next() {
            parts.push(first.to_uppercase().chain(chars).collect::<String>());
        }
    }
    if let Some(tier) = oauth.rate_limit_tier.as_deref().and_then(|t| t.find("max_").map(|i| &t[i + 4..])) {
        parts.push(format!("Max {tier}"));
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_session_weekly_and_model_limits() {
        let body = r#"{
            "five_hour": {"utilization": 15.0},
            "limits": [
                {"kind": "session", "percent": 15, "resets_at": "2026-11-12T12:10:00+00:00", "scope": null},
                {"kind": "weekly_all", "percent": 60, "resets_at": "2026-11-13T23:00:00+00:00", "scope": null},
                {"kind": "weekly_scoped", "percent": 96, "resets_at": null,
                 "scope": {"model": {"id": null, "display_name": "Fable"}, "surface": null}}
            ]
        }"#;
        let limits = parse_limits(body).unwrap();
        let shown: Vec<(&str, f64, &str)> = limits.iter().map(|l| (l.label.as_str(), l.percent, l.level)).collect();
        assert_eq!(shown, [("Session", 15.0, "ok"), ("Week", 60.0, "warn"), ("Fable week", 96.0, "high")]);
        assert_eq!(limits[0].resets_at.as_deref(), Some("2026-11-12T12:10:00+00:00"));
    }

    #[test]
    fn a_reply_without_limits_is_an_error() {
        assert!(parse_limits(r#"{"five_hour": null}"#).is_err());
    }

    #[test]
    fn plan_label_reads_subscription_and_tier() {
        let oauth = OAuth {
            access_token: String::new(),
            expires_at: 0,
            subscription_type: Some("team".into()),
            rate_limit_tier: Some("default_claude_max_5x".into()),
        };
        assert_eq!(plan_label(&oauth).as_deref(), Some("Team · Max 5x"));
    }
}

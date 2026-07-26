//! Subscription quota: GET {base_url}/usages (the endpoint the Kimi Code
//! CLI's own /usage panel reads), parsed into display rows. Used by the
//! REPL's /usage command; kept in the library so the numbers and their
//! quirks (strings-vs-numbers, used-vs-remaining) are tested in one place.

use crate::auth::Auth;
use crate::util::{format_duration, now_secs, parse_rfc3339, truncate_chars};
use serde_json::Value;
use std::time::Duration;

/// One quota row from the usages payload.
pub struct UsageRow {
    pub label: String,
    pub used: i64,
    pub limit: i64,
    pub reset_hint: Option<String>,
}

/// Numbers in the usages payload arrive as JSON strings ("100") or numbers.
pub fn usage_int(v: &Value) -> Option<i64> {
    v.as_i64().or_else(|| v.as_str().and_then(|s| s.parse().ok()))
}

pub fn usage_row(raw: &Value, default_label: &str) -> Option<UsageRow> {
    if !raw.is_object() {
        return None;
    }
    let limit = usage_int(&raw["limit"]);
    let mut used = usage_int(&raw["used"]);
    if used.is_none()
        && let (Some(remaining), Some(limit)) = (usage_int(&raw["remaining"]), limit) {
            used = Some(limit - remaining);
        }
    if used.is_none() && limit.is_none() {
        return None;
    }
    let label = raw["name"]
        .as_str()
        .or_else(|| raw["title"].as_str())
        .unwrap_or(default_label)
        .to_string();
    Some(UsageRow {
        label,
        used: used.unwrap_or(0),
        limit: limit.unwrap_or(0),
        reset_hint: reset_hint(raw),
    })
}

/// Label for one entry of the `limits` array: an explicit name if present,
/// otherwise derived from the window ("300 MINUTE" -> "5h limit").
pub fn limit_label(item: &Value, detail: &Value, idx: usize) -> String {
    for key in ["name", "title", "scope"] {
        for src in [item, detail] {
            if let Some(s) = src[key].as_str()
                && !s.is_empty() {
                    return s.to_string();
                }
        }
    }
    let window = &item["window"];
    let duration = usage_int(&window["duration"])
        .or_else(|| usage_int(&item["duration"]))
        .or_else(|| usage_int(&detail["duration"]));
    let unit = window["timeUnit"]
        .as_str()
        .or_else(|| item["timeUnit"].as_str())
        .or_else(|| detail["timeUnit"].as_str())
        .unwrap_or("");
    if let Some(d) = duration {
        if unit.contains("MINUTE") {
            if d >= 60 && d % 60 == 0 {
                return format!("{}h limit", d / 60);
            }
            return format!("{d}m limit");
        }
        if unit.contains("HOUR") {
            return format!("{d}h limit");
        }
        if unit.contains("DAY") {
            return format!("{d}d limit");
        }
        return format!("{d}s limit");
    }
    format!("Limit #{}", idx + 1)
}

fn reset_hint(raw: &Value) -> Option<String> {
    for key in ["reset_at", "resetAt", "reset_time", "resetTime"] {
        if let Some(v) = raw[key].as_str()
            && !v.is_empty() {
                return Some(format_reset_time(v));
            }
    }
    for key in ["reset_in", "resetIn", "ttl", "window"] {
        if let Some(secs) = usage_int(&raw[key])
            && secs > 0 {
                return Some(format!("resets in {}", format_duration(secs)));
            }
    }
    None
}

fn format_reset_time(val: &str) -> String {
    match parse_rfc3339(val) {
        Some(ts) => {
            let diff = ts - now_secs() as i64;
            if diff <= 0 { "reset".to_string() } else { format!("resets in {}", format_duration(diff)) }
        }
        None => format!("resets at {val}"),
    }
}

/// GET {base_url}/usages with the same bearer credentials as chat; one retry
/// on 401 after refreshing/re-reading credentials.
pub async fn fetch_usage(
    client: &reqwest::Client,
    auth: &mut Auth,
    base_url: &str,
) -> Result<Value, String> {
    let url = format!("{base_url}/usages");
    let mut retried = false;
    loop {
        let token = auth.token(client).await?;
        let resp = client
            .get(&url)
            .bearer_auth(&token)
            .header("Accept", "application/json")
            .header("User-Agent", "kimi-harness/0.2")
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .map_err(|e| format!("network error: {e}"))?;
        if resp.status() == 401 && !retried && auth.handle_401(client).await {
            retried = true;
            continue;
        }
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(format!(
                "usage request failed (HTTP {status}): {}",
                truncate_chars(&body, 300)
            ));
        }
        return resp.json().await.map_err(|e| format!("bad usage response: {e}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_real_payload_shape() {
        let payload = json!({
            "usage": { "limit": "100", "used": "58", "remaining": "42",
                       "resetTime": "2099-07-25T02:16:40.311242Z" },
            "limits": [{
                "window": { "duration": 300, "timeUnit": "TIME_UNIT_MINUTE" },
                "detail": { "limit": "100", "used": "36", "remaining": "64",
                            "resetTime": "2099-07-19T23:16:40.311242Z" }
            }]
        });
        let weekly = usage_row(&payload["usage"], "Weekly limit").unwrap();
        assert_eq!(weekly.label, "Weekly limit");
        assert_eq!((weekly.used, weekly.limit), (58, 100));
        assert!(weekly.reset_hint.unwrap().starts_with("resets"));
        let item = &payload["limits"][0];
        let row = usage_row(&item["detail"], &limit_label(item, &item["detail"], 0)).unwrap();
        assert_eq!(row.label, "5h limit");
        assert_eq!((row.used, row.limit), (36, 100));
    }

    #[test]
    fn used_derived_from_remaining() {
        let row = usage_row(&json!({ "limit": 100, "remaining": 30 }), "x").unwrap();
        assert_eq!(row.used, 70);
        assert!(usage_row(&json!({}), "x").is_none());
    }
}

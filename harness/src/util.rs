//! Small shared helpers: string/number formatting, dates, paths.

use serde_json::Value;

/// Rough local token estimate: ~4 characters per token. Only ever used to
/// *choose* where to compact and to render `/context`; exact counts come back
/// from the API's usage report and are what compaction triggers on.
pub fn est_tokens(chars: usize) -> u64 {
    (chars / 4) as u64
}

/// Characters one message contributes to the context — content, the reasoning
/// the model streamed alongside it, and any tool calls it carries.
pub fn message_chars(message: &Value) -> usize {
    message["content"].as_str().map_or(0, |c| c.chars().count())
        + message["reasoning_content"].as_str().map_or(0, |c| c.chars().count())
        + message["tool_calls"]
            .as_array()
            .map_or(0, |calls| calls.iter().map(|c| c.to_string().chars().count()).sum())
}
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub fn truncate_chars(s: &str, n: usize) -> String {
    if s.chars().count() <= n { s.to_string() } else { s.chars().take(n).collect() }
}

/// 45231 -> "45,231"
pub fn fmt_num(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

pub fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}

/// Civil date from unix time (Howard Hinnant's algorithm), no chrono needed.
pub fn today() -> String {
    let days = (now_secs() / 86_400) as i64;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

/// Days since epoch from a civil date (Hinnant's algorithm; inverse of today()).
pub fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Parse "YYYY-MM-DDTHH:MM:SS[.frac][Z|±HH:MM|±HHMM|±HH]" to unix seconds.
pub fn parse_rfc3339(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 20 || b[4] != b'-' || b[7] != b'-' || !matches!(b[10], b'T' | b't' | b' ') {
        return None;
    }
    let num = |i: usize, n: usize| -> Option<i64> { s.get(i..i + n)?.parse().ok() };
    let (y, mo, d) = (num(0, 4)?, num(5, 2)?, num(8, 2)?);
    let (h, mi, sec) = (num(11, 2)?, num(14, 2)?, num(17, 2)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    let mut i = 19;
    if i < b.len() && b[i] == b'.' {
        i += 1;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
    }
    let mut offset: i64 = 0;
    if i < b.len() {
        match b[i] {
            b'Z' | b'z' => i += 1,
            b'+' | b'-' => {
                let sign: i64 = if b[i] == b'+' { 1 } else { -1 };
                let rest = &s[i + 1..];
                let rb = rest.as_bytes();
                let (oh, om, consumed) = if rb.len() >= 5 && rb[2] == b':' {
                    (num(i + 1, 2)?, num(i + 4, 2)?, 5)
                } else if rb.len() >= 4 && rb[..4].iter().all(u8::is_ascii_digit) {
                    (num(i + 1, 2)?, num(i + 3, 2)?, 4)
                } else if rb.len() >= 2 {
                    (num(i + 1, 2)?, 0, 2)
                } else {
                    return None;
                };
                if oh > 23 || om > 59 {
                    return None;
                }
                offset = sign * (oh * 3_600 + om * 60);
                i += 1 + consumed;
            }
            _ => return None,
        }
    }
    if i != b.len() {
        return None; // trailing garbage
    }
    Some(days_from_civil(y, mo, d) * 86_400 + h * 3_600 + mi * 60 + sec - offset)
}

/// "5d 7h" / "3h 12m" / "45s" — sub-minute precision only when nothing larger.
pub fn format_duration(total: i64) -> String {
    let days = total / 86_400;
    let hours = total % 86_400 / 3_600;
    let mins = total % 3_600 / 60;
    let secs = total % 60;
    let mut parts = Vec::new();
    if days > 0 {
        parts.push(format!("{days}d"));
    }
    if hours > 0 {
        parts.push(format!("{hours}h"));
    }
    if mins > 0 {
        parts.push(format!("{mins}m"));
    }
    if secs > 0 && parts.is_empty() {
        parts.push(format!("{secs}s"));
    }
    if parts.is_empty() { "0s".to_string() } else { parts.join(" ") }
}

/// Extract a required string argument from a tool call's JSON arguments.
pub fn arg_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("error: missing or invalid argument `{key}`"))
}

pub fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").expect("HOME not set"))
}

/// Resolve a user-supplied path against the session working directory: `~`
/// and `~/…` expand to $HOME, relative paths join `cwd`, absolute paths pass
/// through.
pub fn resolve_path(cwd: &Path, path: &str) -> PathBuf {
    if path == "~" {
        return home();
    }
    let expanded = match path.strip_prefix("~/") {
        Some(rest) => home().join(rest),
        None => PathBuf::from(path),
    };
    if expanded.is_absolute() { expanded } else { cwd.join(expanded) }
}

/// One-line summary of a tool call's arguments, for display (`[tool] …`).
pub fn short_args(args: &Value) -> String {
    let Some(map) = args.as_object() else {
        return String::new();
    };
    map.iter()
        .map(|(k, v)| {
            let raw = match v {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            let flat = raw.replace('\n', "\\n");
            let shown = truncate_chars(&flat, 60);
            let ellipsis = if flat.chars().count() > 60 { "…" } else { "" };
            format!("{k}={shown}{ellipsis}")
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_utc() {
        // 1970-01-01T00:00:00Z and a known date: 2026-07-25T02:16:40Z
        assert_eq!(parse_rfc3339("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_rfc3339("2026-07-25T02:16:40Z"), Some(1_784_945_800));
        // Fractional seconds are ignored; lowercase z and space separator ok.
        assert_eq!(parse_rfc3339("2026-07-25T02:16:40.311242Z"), Some(1_784_945_800));
    }

    #[test]
    fn rfc3339_offsets() {
        assert_eq!(parse_rfc3339("1970-01-01T01:00:00+01:00"), Some(0));
        assert_eq!(parse_rfc3339("1970-01-01T01:00:00+0100"), Some(0));
        assert_eq!(parse_rfc3339("1970-01-01T01:00:00+01"), Some(0));
        assert_eq!(parse_rfc3339("1969-12-31T19:00:00-05:00"), Some(0));
    }

    #[test]
    fn rfc3339_rejects_garbage() {
        assert_eq!(parse_rfc3339(""), None);
        assert_eq!(parse_rfc3339("not a date"), None);
        assert_eq!(parse_rfc3339("2026-07-25"), None);
        assert_eq!(parse_rfc3339("2026-13-25T02:16:40Z"), None); // month 13
        assert_eq!(parse_rfc3339("2026-07-25T02:16:40Zzzz"), None); // trailing
        assert_eq!(parse_rfc3339("2026-07-25T25:16:40Z"), None); // hour 25
    }

    #[test]
    fn duration_format() {
        assert_eq!(format_duration(0), "0s");
        assert_eq!(format_duration(45), "45s");
        assert_eq!(format_duration(60), "1m");
        assert_eq!(format_duration(3_600), "1h");
        assert_eq!(format_duration(86_400 + 7 * 3_600 + 5 * 60), "1d 7h 5m");
        assert_eq!(format_duration(-5), "0s");
    }

    #[test]
    fn fmt_num_grouping() {
        assert_eq!(fmt_num(0), "0");
        assert_eq!(fmt_num(612), "612");
        assert_eq!(fmt_num(45231), "45,231");
        assert_eq!(fmt_num(1_000_000), "1,000,000");
    }

    #[test]
    fn resolve_path_schemes() {
        let cwd = Path::new("/work");
        assert_eq!(resolve_path(cwd, "src/main.rs"), PathBuf::from("/work/src/main.rs"));
        assert_eq!(resolve_path(cwd, "/abs/path"), PathBuf::from("/abs/path"));
        let home = home();
        assert_eq!(resolve_path(cwd, "~/x"), home.join("x"));
        assert_eq!(resolve_path(cwd, "~"), home);
    }
}

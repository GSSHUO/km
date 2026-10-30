use crate::auth;
use serde_json::{json, Value};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

#[derive(Clone)]
pub struct QuotaState {
    shared: Arc<(Mutex<QuotaInner>, Condvar)>,
}

struct QuotaInner {
    generation: u64,
    in_flight: Option<u64>,
    snapshot: Option<Value>,
    last_error: Option<String>,
    last_duration_ms: Option<u128>,
    account_hash: Option<String>,
}

fn current_view(inner: &QuotaInner) -> Value {
    if let Some(snapshot) = &inner.snapshot {
        let mut payload = snapshot.clone();
        if let Some(error) = &inner.last_error {
            payload["stale"] = json!(true);
            payload["error"] = json!(error);
        }
        payload["sessionGeneration"] = json!(inner.generation);
        payload
    } else {
        json!({
            "ok": false,
            "error": inner.last_error.clone().unwrap_or_else(|| "正在获取额度".into()),
            "sessionGeneration": inner.generation,
        })
    }
}

impl QuotaState {
    pub fn new() -> Self {
        Self {
            shared: Arc::new((
                Mutex::new(QuotaInner {
                    generation: 0,
                    in_flight: None,
                    snapshot: None,
                    last_error: None,
                    last_duration_ms: None,
                    account_hash: None,
                }),
                Condvar::new(),
            )),
        }
    }

    pub fn reset_session(&self) -> u64 {
        let (lock, wake) = &*self.shared;
        let mut inner = lock.lock().unwrap_or_else(|e| e.into_inner());
        inner.generation += 1;
        inner.snapshot = None;
        inner.last_error = None;
        inner.account_hash = None;
        wake.notify_all();
        inner.generation
    }

    pub fn fetch(&self) -> Value {
        let generation = {
            let (lock, wake) = &*self.shared;
            let mut inner = lock.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                let generation = inner.generation;
                match inner.in_flight {
                    Some(active) if active == generation => {
                        inner = wake.wait(inner).unwrap_or_else(|e| e.into_inner());
                        if inner.generation == generation && inner.in_flight.is_none() {
                            return current_view(&inner);
                        }
                    }
                    _ => {
                        inner.in_flight = Some(generation);
                        break generation;
                    }
                }
            }
        };

        let started = Instant::now();
        let result = auth::fetch_quota_smart_with_identity();
        let duration = started.elapsed().as_millis();
        let (lock, wake) = &*self.shared;
        let mut inner = lock.lock().unwrap_or_else(|e| e.into_inner());
        if inner.in_flight == Some(generation) {
            inner.in_flight = None;
        }
        if generation != inner.generation {
            wake.notify_all();
            return json!({ "ok": false, "staleResponse": true, "error": "账号已变化，已丢弃旧请求", "sessionGeneration": inner.generation });
        }
        inner.last_duration_ms = Some(duration);
        match result {
            Ok((raw, identity)) => {
                if inner
                    .account_hash
                    .as_ref()
                    .is_some_and(|old| old != &identity)
                {
                    inner.generation += 1;
                    inner.snapshot = None;
                }
                inner.account_hash = Some(identity);
                let mut payload = normalize(&raw);
                payload["sessionGeneration"] = json!(inner.generation);
                payload["requestDurationMs"] = json!(duration);
                inner.snapshot = Some(payload.clone());
                inner.last_error = None;
                wake.notify_all();
                payload
            }
            Err(error) => {
                let message = clean_error(&error.to_string());
                let category = if message.contains("过期")
                    || message.contains("重新登录")
                    || message.contains("退出登录")
                {
                    "auth"
                } else if message.contains("请求失败")
                    || message.contains("timeout")
                    || message.contains("网络")
                {
                    "network"
                } else {
                    "service"
                };
                inner.last_error = Some(message.clone());
                wake.notify_all();
                if let Some(snapshot) = &inner.snapshot {
                    let mut payload = snapshot.clone();
                    payload["stale"] = json!(true);
                    payload["error"] = json!(message);
                    payload["errorCategory"] = json!(category);
                    payload["sessionGeneration"] = json!(inner.generation);
                    payload
                } else {
                    json!({ "ok": false, "error": message, "errorCategory": category, "sessionGeneration": inner.generation })
                }
            }
        }
    }

    pub fn diagnostics(&self) -> Value {
        let (lock, _) = &*self.shared;
        let inner = lock.lock().unwrap_or_else(|e| e.into_inner());
        json!({
            "lastSuccessAt": inner.snapshot.as_ref().and_then(|s| s["fetchedAt"].as_u64()),
            "requestDurationMs": inner.last_duration_ms,
            "sessionGeneration": inner.generation,
            "hasSnapshot": inner.snapshot.is_some(),
            "lastError": inner.last_error.as_ref().map(|e| sanitize_diagnostic(e)),
            "inFlight": inner.in_flight.is_some(),
        })
    }

    pub fn summary(&self) -> Result<String, String> {
        let (lock, _) = &*self.shared;
        let inner = lock.lock().unwrap_or_else(|e| e.into_inner());
        let snapshot = inner
            .snapshot
            .clone()
            .ok_or_else(|| "暂无可复制的额度数据".to_string())?;
        let stale = inner.last_error.is_some();
        Ok(format_summary(&snapshot, stale))
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn pick<'a>(raw: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    keys.iter()
        .find_map(|key| raw.get(*key))
        .filter(|value| value.is_object())
}

#[derive(Debug, PartialEq)]
enum Ratio {
    Unknown,
    Valid(f64),
    Invalid,
    OutOfRange(f64),
}

fn ratio_of(stat: Option<&Value>, field: &str) -> Ratio {
    let Some(stat) = stat else {
        return Ratio::Unknown;
    };
    let Some(value) = stat.get(field) else {
        return Ratio::Valid(0.0);
    };
    let parsed = match value {
        Value::Number(number) => number.as_f64(),
        Value::String(text) if !text.trim().is_empty() => text.trim().parse::<f64>().ok(),
        _ => None,
    };
    match parsed {
        Some(value) if value.is_finite() && (0.0..=1.0).contains(&value) => Ratio::Valid(value),
        Some(value) if value.is_finite() => Ratio::OutOfRange(value),
        _ => Ratio::Invalid,
    }
}

fn ratio_json(ratio: Ratio) -> (Value, Value, &'static str) {
    match ratio {
        Ratio::Unknown => (Value::Null, Value::Null, "unknown"),
        Ratio::Valid(value) => (json!(value), Value::Null, "valid"),
        Ratio::Invalid => (Value::Null, json!("比例字段无效"), "invalid"),
        Ratio::OutOfRange(_) => (Value::Null, json!("比例超出有效范围"), "out-of-range"),
    }
}

fn normalized_stat(stat: Option<&Value>, ratio_field: &str, code: bool) -> Value {
    let ratio = ratio_of(stat, ratio_field);
    let (used_ratio, ratio_error, status) = ratio_json(ratio);
    let mut out = json!({
        "usedRatio": used_ratio,
        "resetTime": stat.and_then(|s| s.get("resetTime").or_else(|| s.get("expireTime"))).and_then(Value::as_str),
        "valueStatus": status,
    });
    if code {
        let code_ratio = ratio_of(stat, "kimiCodeUsedRatio");
        let (value, _, code_status) = ratio_json(code_ratio);
        out["codeRatio"] = value;
        out["codeStatus"] = json!(code_status);
    } else {
        out["enabled"] = json!(stat
            .and_then(|s| s.get("enabled"))
            .and_then(Value::as_bool)
            .unwrap_or(true));
    }
    if !ratio_error.is_null() {
        out["valueError"] = ratio_error;
    }
    out
}

pub fn normalize(raw: &Value) -> Value {
    let balance = pick(raw, &["subscriptionBalance"]);
    let five = pick(raw, &["ratelimitCode5h", "ratelimit5h"]);
    let seven = pick(raw, &["ratelimitCode7d", "ratelimit7d"]);
    json!({
        "ok": true,
        "fetchedAt": now_secs(),
        "total": normalized_stat(balance, "amountUsedRatio", true),
        "fiveHour": normalized_stat(five, "ratio", false),
        "sevenDay": normalized_stat(seven, "ratio", false),
    })
}

pub fn clean_error(message: &str) -> String {
    message
        .trim_start_matches("AUTH_DEAD: ")
        .trim_start_matches("AUTH_EXPIRED: ")
        .to_string()
}

fn sanitize_diagnostic(message: &str) -> String {
    let mut out = clean_error(message)
        .split_whitespace()
        .map(|token| {
            let probe = token.trim_matches(|c| matches!(c, '"' | '\'' | '(' | ')' | ',' | ';'));
            if probe.starts_with('/') || probe.contains(":\\") || probe.starts_with("http") {
                "[已移除路径或地址]"
            } else if probe.starts_with("eyJ") {
                "[已移除凭据]"
            } else {
                token
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    if out.len() > 240 {
        out = out.chars().take(240).collect();
    }
    out
}

fn percent(value: &Value) -> String {
    value
        .as_f64()
        .filter(|v| v.is_finite())
        .map(|v| format!("{:.2}%", v * 100.0))
        .unwrap_or_else(|| "—".into())
}

fn reset_text(value: &Value) -> String {
    value
        .as_str()
        .and_then(|iso| {
            let time = chrono_like_iso(iso)?;
            Some(if time <= now_secs() as i64 {
                "等待更新".into()
            } else {
                format_relative(time - now_secs() as i64)
            })
        })
        .unwrap_or_else(|| "重置时间未知".into())
}

fn chrono_like_iso(iso: &str) -> Option<i64> {
    // The UI performs full timezone-aware formatting. Summary keeps the source
    // timestamp when a minimal parser cannot safely interpret it.
    if iso.len() < 19 {
        return None;
    }
    let year: i64 = iso.get(0..4)?.parse().ok()?;
    let month: i64 = iso.get(5..7)?.parse().ok()?;
    let day: i64 = iso.get(8..10)?.parse().ok()?;
    let hour: i64 = iso.get(11..13)?.parse().ok()?;
    let minute: i64 = iso.get(14..16)?.parse().ok()?;
    let second: i64 = iso.get(17..19)?.parse().ok()?;
    let days = days_from_civil(year, month, day);
    let mut timestamp = days * 86400 + hour * 3600 + minute * 60 + second;
    if iso.ends_with('Z') {
        return Some(timestamp);
    }
    let sign_pos = iso
        .get(19..)?
        .find(|c| c == '+' || c == '-')
        .map(|p| p + 19);
    if let Some(pos) = sign_pos {
        let sign = if iso.as_bytes()[pos] == b'-' { -1 } else { 1 };
        let oh: i64 = iso.get(pos + 1..pos + 3)?.parse().ok()?;
        let om: i64 = iso.get(pos + 4..pos + 6)?.parse().ok()?;
        timestamp -= sign * (oh * 3600 + om * 60);
    }
    Some(timestamp)
}

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = year - if month <= 2 { 1 } else { 0 };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = month + if month > 2 { -3 } else { 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn format_relative(seconds: i64) -> String {
    let minutes = seconds / 60;
    let hours = minutes / 60;
    let days = hours / 24;
    if days >= 1 {
        format!("{days}天{}小时后重置", hours % 24)
    } else if hours >= 1 {
        format!("{hours}小时{}分后重置", minutes % 60)
    } else {
        format!("{}分钟后重置", minutes.max(0))
    }
}

fn format_data_time(seconds: u64) -> String {
    // Avoid inventing a timezone: Unix time is unambiguous and contains no
    // account or machine information. The card itself shows local time.
    format!("Unix {seconds}")
}

pub fn format_summary(snapshot: &Value, stale: bool) -> String {
    let total = &snapshot["total"];
    let five = &snapshot["fiveHour"];
    let seven = &snapshot["sevenDay"];
    let mut lines = vec![
        "Kimi 额度（已用）".to_string(),
        format!(
            "月度 {} · {}",
            percent(&total["usedRatio"]),
            reset_text(&total["resetTime"])
        ),
        format!(
            "5小时 {} · {}",
            if five["enabled"] == false {
                "—".into()
            } else {
                percent(&five["usedRatio"])
            },
            if five["enabled"] == false {
                "未启用".into()
            } else {
                reset_text(&five["resetTime"])
            }
        ),
        format!(
            "7天 {} · {}",
            if seven["enabled"] == false {
                "—".into()
            } else {
                percent(&seven["usedRatio"])
            },
            if seven["enabled"] == false {
                "未启用".into()
            } else {
                reset_text(&seven["resetTime"])
            }
        ),
        format!("Kimi Code {}（接口返回占比）", percent(&total["codeRatio"])),
        format!(
            "数据时间 {}",
            format_data_time(snapshot["fetchedAt"].as_u64().unwrap_or(0))
        ),
    ];
    if stale {
        lines.push("上次数据，暂未刷新".into());
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_ratio_on_present_stat_is_zero() {
        let value = normalize(&json!({ "subscriptionBalance": {} }));
        assert_eq!(value["total"]["usedRatio"], 0.0);
        assert_eq!(value["total"]["valueStatus"], "valid");
        assert_eq!(value["fiveHour"]["valueStatus"], "unknown");
    }

    #[test]
    fn null_and_wrong_types_are_invalid_not_zero() {
        for raw in [
            json!({ "subscriptionBalance": { "amountUsedRatio": null } }),
            json!({ "subscriptionBalance": { "amountUsedRatio": "" } }),
            json!({ "subscriptionBalance": { "amountUsedRatio": true } }),
        ] {
            let value = normalize(&raw);
            assert_eq!(value["total"]["usedRatio"], Value::Null);
            assert_eq!(value["total"]["valueStatus"], "invalid");
        }
    }

    #[test]
    fn numeric_strings_are_supported_and_out_of_range_is_flagged() {
        let value = normalize(&json!({ "subscriptionBalance": { "amountUsedRatio": "0.25" } }));
        assert_eq!(value["total"]["usedRatio"], 0.25);
        let value = normalize(&json!({ "subscriptionBalance": { "amountUsedRatio": 1.2 } }));
        assert_eq!(value["total"]["valueStatus"], "out-of-range");
        assert_eq!(value["total"]["usedRatio"], Value::Null);
    }

    #[test]
    fn stale_summary_is_whitelisted() {
        let mut snapshot = normalize(&json!({ "subscriptionBalance": { "amountUsedRatio": 0.5 } }));
        snapshot["fetchedAt"] = json!(123);
        let text = format_summary(&snapshot, true);
        assert!(text.contains("Kimi 额度（已用）"));
        assert!(text.contains("上次数据，暂未刷新"));
        assert!(!text.contains("amountUsedRatio"));
    }
}

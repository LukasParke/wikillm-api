//! Trust-tier derivation and actor naming for OKF bundles.

use chrono::{DateTime, NaiveDate, Utc};
use serde_json::Value;

use crate::domain::{trust_tier, VerifiedEntry};

/// Derive the trust tier from normalized verification entries, per
/// [`crate::domain::TRUST_ORDER`]: `unverified`, `machine-confirmed`, or
/// `human-reviewed`.
pub fn derive_trust_tier(verified: Option<&Vec<VerifiedEntry>>) -> &'static str {
    trust_tier(verified)
}

/// Accepts a list of verification entries or a bare `{by, at}` mapping
/// (spec §5.2); returns normalized entries or `None` for absent/empty.
/// Entries missing a non-empty `by` are dropped; `at` defaults to `""`.
pub fn normalize_verified(verified: &Value) -> Option<Vec<VerifiedEntry>> {
    let raw_entries: Vec<&Value> = match verified {
        Value::Null => return None,
        Value::Array(items) => items.iter().collect(),
        bare => vec![bare],
    };
    let mut out = Vec::new();
    for raw in raw_entries {
        let Some(obj) = raw.as_object() else {
            continue;
        };
        let Some(by) = obj.get("by").and_then(Value::as_str) else {
            continue;
        };
        if by.is_empty() {
            continue;
        }
        let at = obj.get("at").and_then(Value::as_str).unwrap_or("");
        out.push(VerifiedEntry {
            by: by.to_string(),
            at: at.to_string(),
        });
    }
    if out.is_empty() { None } else { Some(out) }
}

fn parse_timestamp(raw: &str) -> Option<DateTime<Utc>> {
    if let Ok(dt) = DateTime::parse_from_rfc3339(raw) {
        return Some(dt.into());
    }
    match NaiveDate::parse_from_str(raw, "%Y-%m-%d") {
        Ok(date) => date.and_hms_opt(0, 0, 0).map(|dt| dt.and_utc()),
        Err(_) => None,
    }
}

/// True when `stale_after` parses to a timestamp at or before `now`.
/// Absent, empty, or invalid values are never stale.
pub fn is_stale(stale_after: Option<&str>, now: DateTime<Utc>) -> bool {
    let Some(raw) = stale_after else {
        return false;
    };
    if raw.is_empty() {
        return false;
    }
    parse_timestamp(raw).is_some_and(|ts| ts <= now)
}

/// Spec §7 actor convention: human actors get `human:<name>`; machine sources
/// get `<source>/wikillm-api`.
pub fn actor_from_source(source_name: &str, human_actors: &[String]) -> String {
    let lowered = source_name.to_lowercase();
    if human_actors.iter().any(|actor| actor.to_lowercase() == lowered) {
        return format!("human:{source_name}");
    }
    if lowered.starts_with("user-") || lowered.starts_with("human-") {
        return format!("human:{source_name}");
    }
    format!("{source_name}/wikillm-api")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn normalizes_bare_mapping() {
        let bare = normalize_verified(&json!({ "by": "bot" })).unwrap();
        assert_eq!(bare.len(), 1);
        assert_eq!(bare[0].by, "bot");
        assert_eq!(bare[0].at, "");
        assert!(normalize_verified(&json!(["junk", { "at": "x" }])).is_none());
    }

    #[test]
    fn staleness() {
        let now = Utc::now();
        assert!(is_stale(Some("2020-01-01T00:00:00Z"), now));
        assert!(!is_stale(Some("2999-01-01T00:00:00Z"), now));
        assert!(!is_stale(Some("not-a-date"), now));
        assert!(!is_stale(None, now));
    }
}

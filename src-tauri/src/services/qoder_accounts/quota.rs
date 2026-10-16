use super::{Account, QuotaWindow};
use chrono::Datelike;
use serde_json::Value;

pub(super) enum Failure {
    Expired,
    Network,
    Format,
}
impl Failure {
    pub(super) fn message(self) -> String {
        match self {
            Self::Expired => "accountError.loginRequired",
            Self::Network => "accountError.network",
            Self::Format => "accountError.format",
        }
        .into()
    }
}
pub(super) struct Quota {
    plan: Option<String>,
    expires: Option<i64>,
    windows: Vec<QuotaWindow>,
}
impl Quota {
    pub(super) fn apply(self, account: &mut Account) {
        account.plan = self.plan;
        account.expires_at = self.expires;
        account.total = if self.windows.is_empty() {
            None
        } else {
            self.windows.iter().map(|w| w.total).sum()
        };
        account.remaining = if self.windows.is_empty() {
            None
        } else {
            self.windows.iter().map(|w| w.remaining).sum()
        };
        account.quota_windows = self.windows;
    }
}
pub(super) fn timestamp(v: &Value) -> Option<i64> {
    if let Some(s) = v.as_str() {
        if let Ok(date) = chrono::DateTime::parse_from_rfc3339(s) {
            return Some(date.timestamp());
        }
    }
    let n = v
        .as_i64()
        .or_else(|| v.as_str()?.parse::<i64>().ok())?
        .max(0);
    (n > 0).then_some(if n >= 1_000_000_000_000 { n / 1000 } else { n })
}
pub(super) fn normalize_expiry(seconds: Option<i64>) -> Option<i64> {
    seconds.filter(|seconds| {
        *seconds > 0
            && chrono::DateTime::from_timestamp(*seconds, 0).is_some_and(|date| date.year() < 9999)
    })
}
fn expiry(value: &Value) -> Option<i64> {
    normalize_expiry(timestamp(value))
}
fn number(v: &Value) -> Option<f64> {
    v.as_f64().filter(|n| n.is_finite() && *n >= 0.0)
}
fn alias<'a>(v: &'a Value, a: &str, b: &str) -> &'a Value {
    v.get(a).or_else(|| v.get(b)).unwrap_or(&Value::Null)
}
fn window(v: &Value, label: &str) -> Option<QuotaWindow> {
    if !v.is_object() {
        return None;
    }
    let total = number(alias(v, "total", "cap"));
    let remaining =
        number(&v["remaining"]).or_else(|| Some((total? - number(&v["used"])?).max(0.0)));
    if total.is_none() && remaining.is_none() {
        return None;
    }
    Some(QuotaWindow {
        name: v["name"]
            .as_str()
            .filter(|s| !s.is_empty())
            .unwrap_or(label)
            .into(),
        total,
        remaining,
        expires_at: expiry(alias(v, "expiresAt", "expires_at")),
    })
}
fn parse(env: &Value) -> Result<Quota, Failure> {
    if env["displayMode"] == "enterprise" {
        return Ok(Quota {
            plan: Some("Enterprise".into()),
            expires: None,
            windows: vec![],
        });
    }
    let value = &env["qoderUsage"];
    if env["displayMode"] != "qoder" || !value.is_object() {
        return Err(Failure::Format);
    }
    let mut windows = vec![];
    for (a, b, label) in [
        ("userQuota", "user_quota", "Credits"),
        ("addOnQuota", "add_on_quota", "Add-on"),
        ("orgResourcePackage", "org_resource_package", "Organization"),
    ] {
        if let Some(w) = window(alias(value, a, b), label) {
            windows.push(w);
        }
    }
    for v in alias(
        value,
        "dedicatedResourcePackages",
        "dedicated_resource_packages",
    )
    .as_array()
    .into_iter()
    .flatten()
    {
        if let Some(w) = window(v, "Dedicated") {
            windows.push(w);
        }
    }
    Ok(Quota {
        plan: alias(value, "userType", "user_type")
            .as_str()
            .map(str::to_string),
        expires: expiry(alias(value, "expiresAt", "expires_at")),
        windows,
    })
}
pub(super) async fn fetch(
    client: &reqwest::Client,
    base: &str,
    token: &str,
) -> Result<Quota, Failure> {
    let response = client
        .get(format!("{base}/sash/api/v2/me/usage"))
        .bearer_auth(token)
        .header("Accept", "application/json")
        .header("Cosy-ClientType", "10")
        .header("User-Agent", "Qoder")
        .send()
        .await
        .map_err(|_| Failure::Network)?;
    if matches!(response.status().as_u16(), 401 | 403) {
        return Err(Failure::Expired);
    }
    if !response.status().is_success() {
        return Err(Failure::Network);
    }
    parse(
        &response
            .json::<Value>()
            .await
            .map_err(|_| Failure::Format)?,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn permanent_quota_dates_never_become_countdowns_or_change_token_expiry() {
        for date in [
            json!(253_402_214_400i64),
            json!(253_402_214_400_000i64),
            json!("9999-12-31T00:00:00Z"),
        ] {
            assert_eq!(timestamp(&date), Some(253_402_214_400));
            let quota = parse(&json!({"displayMode":"qoder","qoderUsage":{
                "userType":"personal_standard","expiresAt":date,
                "userQuota":{"total":0,"remaining":0},
                "addOnQuota":{"total":300,"remaining":300,"expiresAt":date}
            }}))
            .ok()
            .unwrap();
            assert_eq!(quota.expires, None);
            assert_eq!(quota.plan.as_deref(), Some("personal_standard"));
            assert_eq!(quota.windows[0].remaining, Some(0.0));
            assert_eq!(quota.windows[1].remaining, Some(300.0));
            assert_eq!(quota.windows[1].expires_at, None);
        }
    }

    #[test]
    fn real_subscription_and_bucket_deadlines_are_independent() {
        for date in [
            json!(1_893_456_000i64),
            json!(1_893_456_000_000i64),
            json!("2030-01-01T00:00:00Z"),
        ] {
            let quota = parse(&json!({"displayMode":"qoder","qoderUsage":{
                "userType":"personal_professional","expiresAt":date,
                "userQuota":{"total":100,"remaining":50},
                "addOnQuota":{"total":300,"remaining":300},
                "dedicatedResourcePackages":[{"name":"Fixture","total":20,"remaining":10,"expiresAt":1_893_400_000}]
            }})).ok().unwrap();
            assert_eq!(quota.expires, Some(1_893_456_000));
            assert_eq!(quota.windows[0].expires_at, None);
            assert_eq!(quota.windows[1].expires_at, None);
            assert_eq!(quota.windows[2].expires_at, Some(1_893_400_000));
        }
    }

    #[test]
    fn pools_preserve_unknowns_and_real_zero_without_fabricating_quota() {
        let quota=parse(&json!({"displayMode":"qoder","qoderUsage":{"userType":"Pro","expiresAt":1_893_456_000_000i64,"userQuota":{"total":100,"used":100},"addOnQuota":{"remaining":20},"dedicatedResourcePackages":[{"name":"Pool","total":40,"remaining":30}]}})).ok().unwrap();
        assert_eq!(quota.expires, Some(1_893_456_000));
        assert_eq!(quota.windows[0].remaining, Some(0.0));
        assert_eq!(quota.windows[1].total, None);
        assert_eq!(quota.windows[2].name, "Pool");
        let unknown = parse(&json!({"displayMode":"qoder","qoderUsage":{}}))
            .ok()
            .unwrap();
        assert!(unknown.windows.is_empty());
        assert!(parse(&json!({"displayMode":"qoder"})).is_err());
    }
}

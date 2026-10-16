//! AI Pulse archive — per-day JSON files on disk.
//!
//! Layout: `~/.echobird/pulse/YYYY/MM/DD_{lang}.json`
//!
//! Each file:
//! ```json
//! { "schema": 1, "date": "2026-05-15", "lang": "zh",
//!   "item_count": 538, "items": [ ...NewsItem... ] }
//! ```
//!
//! Upstream feeds give a sliding 7-day window: missed days are lost
//! forever, so every fetch fans the items out into per-day buckets
//! (by local date of `published_at`) and atomically merges-with-existing
//! on disk. Result: as long as the user opens EchoBird at least every
//! 7 days, no day is dropped.
//!
//! Atomic write = `tmp file → rename`. A partial write never leaves a
//! day's archive in a half-parsed state.

use chrono::{Datelike, Duration, Local, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::utils::platform::echobird_dir;

/// Some upstream aggregators (newsnow + juejin in particular, a few wechat
/// scrapers occasionally) stamp Beijing time as if it were UTC, so a story
/// published at 16:24 CST gets written as "2026-05-23T16:24:59Z" — 8h in
/// the future relative to the snapshot's own clock. The fix lives in
/// `scripts/filter_pulse.py` on the bot side, but it doesn't repair archive
/// files we already wrote with the bad data: those rows live in a
/// "future date" bucket file (e.g. `2026/05/24_zh.json` on a CST machine)
/// that the corrected upstream payload never touches, so cross-file dedupe
/// in `load_all` lets the bad row keep winning forever.
///
/// `effective_ts` mirrors the frontend's `itemTs()` future-guard: when
/// `published_at` parses to >now+5min it falls back to `first_seen_at` /
/// `last_seen_at`. `bucket_date` (write path) and `load_all` (read path)
/// both call through this so:
///   1. New writes never land in a future date file.
///   2. Pre-existing future date files are dropped at load time by the
///      `date > today+1` filter in `load_all`.
const FUTURE_SLACK_SECS: i64 = 5 * 60;

fn effective_ts(item: &NewsItem, now_plus_slack_ts: i64) -> &str {
    let pub_str = item.published_at.as_deref().unwrap_or("");
    if pub_str.is_empty()
        || chrono::DateTime::parse_from_rfc3339(pub_str)
            .is_ok_and(|dt| dt.timestamp() > now_plus_slack_ts)
    {
        return item
            .first_seen_at
            .as_deref()
            .filter(|s| !s.is_empty())
            .or_else(|| item.last_seen_at.as_deref().filter(|s| !s.is_empty()))
            .unwrap_or(pub_str);
    }
    pub_str
}

/// Mirror of the frontend's `NewsItem` shape. `serde(default)` on every
/// optional field so a future upstream-schema tweak that drops or renames
/// fields doesn't break archive reads.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewsItem {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub site_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub site_name: Option<String>,
    pub source: String,
    pub title: String,
    pub url: String,
    #[serde(default)]
    pub published_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_seen_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_seen_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_zh: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_en: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct DayFile {
    #[serde(default = "default_schema")]
    schema: u32,
    date: String,
    lang: String,
    #[serde(default)]
    item_count: usize,
    items: Vec<NewsItem>,
}

fn default_schema() -> u32 {
    1
}

const PULSE_SUBDIR: &str = "pulse";

fn pulse_root() -> PathBuf {
    echobird_dir().join(PULSE_SUBDIR)
}

/// Build the on-disk path for a (date, lang) pair. Returns `None` if
/// `date` isn't a well-formed `YYYY-MM-DD` — caller's responsibility to
/// skip the item rather than write to a path with embedded garbage.
fn day_path(root: &Path, date: &str, lang: &str) -> Option<PathBuf> {
    if date.len() != 10 || !date.is_ascii() || !matches!(lang, "zh" | "en") {
        return None;
    }
    chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()?;
    Some(
        root.join(&date[0..4])
            .join(&date[5..7])
            .join(format!("{}_{}.json", &date[8..10], lang)),
    )
}

/// Pick the YYYY-MM-DD bucket for an item using the user's local
/// timezone. Falls back: `effective_ts` (= published_at with future-guard)
/// → `first_seen_at` → `last_seen_at` → today. Without the timezone
/// conversion a naive `ts.slice(0,10)` lumps every CST 00:00–08:00 item
/// into the wrong (UTC) bucket — same bug the TS code at `itemLocalDate`
/// already fixes. The `effective_ts` indirection guarantees that a
/// future-stamped row goes into its `first_seen_at` day bucket instead
/// of a phantom future-date file.
fn bucket_date(item: &NewsItem, today_str: &str, now_plus_slack_ts: i64) -> String {
    let ts = effective_ts(item, now_plus_slack_ts);
    if ts.is_empty() {
        return today_str.to_string();
    }

    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(ts) {
        let local = dt.with_timezone(&Local);
        return format!(
            "{:04}-{:02}-{:02}",
            local.year(),
            local.month(),
            local.day()
        );
    }

    // Last resort: if the string at least starts with "YYYY-MM-DD" we
    // trust that prefix. Loses timezone info but matches the legacy
    // localStorage bucketing — better than dumping everything into today.
    if let Some(head) = ts.get(..10) {
        if head.chars().enumerate().all(|(i, c)| match i {
            4 | 7 => c == '-',
            _ => c.is_ascii_digit(),
        }) {
            return head.to_string();
        }
    }

    today_str.to_string()
}

/// Fan items out into per-day buckets, merge with whatever's already on
/// disk (dedupe by url, newer wins), atomic-write each bucket back.
///
/// Idempotent: re-running with the same input is a no-op once the data
/// is on disk. That property is why we can call this on every refresh
/// without worrying about double-counting.
pub fn save_fanout(lang: &str, items: Vec<NewsItem>) -> Result<usize, String> {
    save_fanout_at(&pulse_root(), lang, items)
}

fn save_fanout_at(root: &Path, lang: &str, items: Vec<NewsItem>) -> Result<usize, String> {
    if items.is_empty() {
        return Ok(0);
    }

    let now = Local::now();
    let today_str = format!("{:04}-{:02}-{:02}", now.year(), now.month(), now.day());
    let now_plus_slack = Utc::now().timestamp() + FUTURE_SLACK_SECS;

    let mut buckets: HashMap<String, Vec<NewsItem>> = HashMap::new();
    for it in items {
        let date = bucket_date(&it, &today_str, now_plus_slack);
        buckets.entry(date).or_default().push(it);
    }

    let mut total_written = 0usize;
    for (date, new_items) in buckets {
        let Some(path) = day_path(root, &date, lang) else {
            log::warn!("[PulseArchive] skipping malformed bucket: {}", date);
            continue;
        };

        // Load existing items for this date (if any).
        let existing_items: Vec<NewsItem> = match fs::read_to_string(&path) {
            Ok(content) => {
                serde_json::from_str::<DayFile>(&content)
                    .map_err(|e| format!("{} parse error: {}", path.display(), e))?
                    .items
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(format!("{} read error: {}", path.display(), e)),
        };

        // Dedupe by url. The HashMap insert semantics mean: incoming new
        // item replaces the cached one for the same url — that's
        // desirable because upstream sometimes back-fills better
        // translations / titles, and we want the freshest copy.
        let mut by_url: HashMap<String, NewsItem> = HashMap::new();
        for it in existing_items {
            by_url.insert(it.url.clone(), it);
        }
        for it in new_items {
            by_url.insert(it.url.clone(), it);
        }

        let mut merged: Vec<NewsItem> = by_url.into_values().collect();
        // Newest-first for human-friendly file inspection. Empty strings
        // sort to the bottom, which is also what we want for items
        // missing every timestamp. Sort key uses effective_ts so a
        // future-stamped row doesn't claim the top slot.
        merged.sort_by(|a, b| {
            let ka = effective_ts(a, now_plus_slack);
            let kb = effective_ts(b, now_plus_slack);
            kb.cmp(ka)
        });

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("mkdir failed: {}", e))?;
        }

        let day_file = DayFile {
            schema: 1,
            date: date.clone(),
            lang: lang.to_string(),
            item_count: merged.len(),
            items: merged,
        };

        // serde_json::to_string (compact) keeps each day-file under
        // 200 KB even on heavy days — pretty-printing would double size
        // for no gain, since these files are read by code 99% of the time.
        let json = serde_json::to_string(&day_file).map_err(|e| format!("encode: {}", e))?;

        let tmp_path = path.with_extension("json.tmp");
        fs::write(&tmp_path, json).map_err(|e| format!("write tmp: {}", e))?;
        fs::rename(&tmp_path, &path).map_err(|e| format!("rename: {}", e))?;

        total_written += day_file.item_count;
    }

    Ok(total_written)
}

/// Load every item we have on disk for a given lang, sorted newest-first.
///
/// Yes, "every" — current archive volumes (50 MB / year for the lightest
/// news-item shape) make global loading cheap enough that the simplicity
/// is worth more than lazy windowing. If users ever break 5+ years of
/// archive we can revisit, but that's a multi-year-from-now problem.
pub fn load_all(lang: &str) -> Result<Vec<NewsItem>, String> {
    load_all_at(&pulse_root(), lang)
}

fn load_all_at(root: &Path, lang: &str) -> Result<Vec<NewsItem>, String> {
    // Self-heal: drop any date file dated more than 1 day past today in
    // the user's local TZ. These can only exist as residue from the
    // pre-fix upstream bug where future-stamped items got bucketed into
    // phantom future dates (e.g. 2026/05/24_zh.json holding a single
    // 2026-05-23T16:24:59Z-stamped row on a CST machine). Skipping the
    // file at load time keeps the bad URL out of seen_urls so the real
    // entry from the correct day bucket wins the cross-file dedupe. The
    // file itself stays on disk and goes cold once the URL drops out of
    // the upstream 7-day window — not worth deleting eagerly.
    let now_local = Local::now();
    let tomorrow_local = now_local + Duration::days(1);
    let max_date_str = format!(
        "{:04}-{:02}-{:02}",
        tomorrow_local.year(),
        tomorrow_local.month(),
        tomorrow_local.day()
    );

    let dates: Vec<String> = list_all_dates(root, lang)?
        .into_iter()
        .filter(|d| d.as_str() <= max_date_str.as_str())
        .collect();
    let mut out: Vec<NewsItem> = Vec::new();
    let mut seen_urls: HashSet<String> = HashSet::new();

    for date in &dates {
        let Some(path) = day_path(root, date, lang) else {
            continue;
        };
        let content = fs::read_to_string(&path)
            .map_err(|e| format!("{} read error: {}", path.display(), e))?;
        let df: DayFile = serde_json::from_str(&content)
            .map_err(|e| format!("{} parse error: {}", path.display(), e))?;
        for it in df.items {
            // Cross-day url dedupe — a freak upstream timestamp change
            // could land the same URL in two day files; this guards
            // against double-display.
            if seen_urls.insert(it.url.clone()) {
                out.push(it);
            }
        }
    }

    // Sort key uses effective_ts (with future-guard) so a row whose
    // published_at is in the future doesn't claim the top slot. Matches
    // the frontend `itemTs()` so display order and label agree.
    let now_plus_slack = Utc::now().timestamp() + FUTURE_SLACK_SECS;
    out.sort_by(|a, b| {
        let ka = effective_ts(a, now_plus_slack);
        let kb = effective_ts(b, now_plus_slack);
        kb.cmp(ka)
    });

    Ok(out)
}

/// Enumerate every `YYYY-MM-DD` we have a file for, desc.
fn list_all_dates(root: &Path, lang: &str) -> Result<Vec<String>, String> {
    let mut dates: Vec<String> = Vec::new();

    let year_entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(dates),
        Err(e) => return Err(format!("{} read error: {}", root.display(), e)),
    };
    let lang_suffix = format!("_{}.json", lang);

    for ye in year_entries {
        let ye = ye.map_err(|e| format!("archive directory entry: {}", e))?;
        let yname = ye.file_name().to_string_lossy().into_owned();
        if yname.len() != 4 || !yname.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        if !ye.file_type().map_err(|e| e.to_string())?.is_dir() {
            continue;
        }
        let month_entries = fs::read_dir(ye.path()).map_err(|e| e.to_string())?;
        for me in month_entries {
            let me = me.map_err(|e| e.to_string())?;
            let mname = me.file_name().to_string_lossy().into_owned();
            if mname.len() != 2 || !mname.chars().all(|c| c.is_ascii_digit()) {
                continue;
            }
            if !me.file_type().map_err(|e| e.to_string())?.is_dir() {
                continue;
            }
            let day_entries = fs::read_dir(me.path()).map_err(|e| e.to_string())?;
            for de in day_entries {
                let de = de.map_err(|e| e.to_string())?;
                let fname = de.file_name().to_string_lossy().into_owned();
                let Some(day_part) = fname.strip_suffix(&lang_suffix) else {
                    continue;
                };
                if day_part.len() != 2 || !day_part.chars().all(|c| c.is_ascii_digit()) {
                    continue;
                }
                dates.push(format!("{}-{}-{}", yname, mname, day_part));
            }
        }
    }

    dates.sort();
    dates.reverse();
    Ok(dates)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, timestamp: &str) -> NewsItem {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "title": id,
            "source": "AI Lab",
            "url": format!("https://example.com/{id}"),
            "published_at": timestamp
        }))
        .unwrap()
    }

    #[test]
    fn archive_merges_by_url_survives_reload_and_separates_languages() {
        let root = std::env::temp_dir().join(format!("echobird-pulse-{}", uuid::Uuid::new_v4()));
        let now = Local::now();
        let today = now.format("%Y-%m-%d").to_string();
        let yesterday = (now - Duration::days(1)).format("%Y-%m-%d").to_string();
        let first = item("article", &now.to_rfc3339());
        let older = item("older", &(now - Duration::days(1)).to_rfc3339());
        save_fanout_at(&root, "zh", vec![first.clone(), older.clone()]).unwrap();
        let mut updated = first;
        updated.title = "Updated title".into();
        save_fanout_at(&root, "zh", vec![updated.clone()]).unwrap();
        save_fanout_at(&root, "en", vec![older]).unwrap();

        let loaded = load_all_at(&root, "zh").unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].title, updated.title);
        assert_eq!(
            list_all_dates(&root, "zh").unwrap(),
            vec![today.clone(), yesterday]
        );
        assert_eq!(load_all_at(&root, "en").unwrap().len(), 1);
        let json = fs::read_to_string(day_path(&root, &today, "zh").unwrap()).unwrap();
        let day: DayFile = serde_json::from_str(&json).unwrap();
        assert_eq!(day.item_count, 1);
        assert!(!day_path(&root, &today, "zh")
            .unwrap()
            .with_extension("json.tmp")
            .exists());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn missing_archive_is_empty_and_empty_save_does_not_create_it() {
        let root = std::env::temp_dir().join(format!("echobird-pulse-{}", uuid::Uuid::new_v4()));
        assert!(load_all_at(&root, "zh").unwrap().is_empty());
        assert_eq!(save_fanout_at(&root, "zh", vec![]).unwrap(), 0);
        assert!(!root.exists());
    }

    #[test]
    fn paths_reject_invalid_dates_and_languages() {
        let root = Path::new("archive");
        for date in [
            "",
            "2026-13-40",
            "2026/10/14",
            "资讯日期测试",
            "2026-10-14/../x",
        ] {
            assert!(day_path(root, date, "zh").is_none());
        }
        assert!(day_path(root, "2026-10-14", "../en").is_none());
        assert!(day_path(root, "2026-10-14", "zh").is_some());
    }

    #[test]
    fn dates_use_local_timezone_and_future_timestamp_falls_back_to_first_seen() {
        let seen = Utc::now();
        let mut news = item("article", &(seen + Duration::hours(8)).to_rfc3339());
        news.first_seen_at = Some(seen.to_rfc3339());
        let expected = seen.with_timezone(&Local).format("%Y-%m-%d").to_string();
        assert_eq!(
            bucket_date(&news, "unused", seen.timestamp() + 300),
            expected
        );
        news.published_at = None;
        assert_eq!(
            bucket_date(&news, "unused", seen.timestamp() + 300),
            expected
        );
    }

    #[test]
    fn refresh_does_not_overwrite_a_damaged_day_archive() {
        let root = std::env::temp_dir().join(format!("echobird-pulse-{}", uuid::Uuid::new_v4()));
        let now = Local::now();
        let date = now.format("%Y-%m-%d").to_string();
        let path = day_path(&root, &date, "zh").unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "damaged archive with recoverable history").unwrap();
        assert!(load_all_at(&root, "zh").is_err());
        assert!(save_fanout_at(&root, "zh", vec![item("article", &now.to_rfc3339())]).is_err());
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "damaged archive with recoverable history"
        );
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn missing_publication_uses_first_seen_for_newest_first_order() {
        let root = std::env::temp_dir().join(format!("echobird-pulse-{}", uuid::Uuid::new_v4()));
        let now = Local::now();
        let mut newest = item("newest", &now.to_rfc3339());
        newest.first_seen_at = newest.published_at.take();
        let older = item("older", &(now - Duration::hours(1)).to_rfc3339());
        save_fanout_at(&root, "zh", vec![older, newest]).unwrap();
        assert_eq!(load_all_at(&root, "zh").unwrap()[0].id, "newest");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn malformed_unicode_timestamp_does_not_panic() {
        let news = item("article", "日期格式无效");
        assert_eq!(bucket_date(&news, "2026-10-14", i64::MAX), "2026-10-14");
    }
}

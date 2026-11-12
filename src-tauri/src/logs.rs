//! Reads Claude Code's session logs (`~/.claude/projects/**/*.jsonl`) and totals token use.
//! The logs are append-only, so after the first pass only the new bytes of each file are read.

use crate::pricing::{self, Billable};
use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

const BURN_WINDOW_MINUTES: i64 = 15;

#[derive(Default, Clone, Copy, Serialize, Debug, PartialEq)]
pub struct Tokens {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

impl Tokens {
    pub fn total(&self) -> u64 {
        self.input + self.output + self.cache_read + self.cache_write
    }

    fn add(&mut self, other: &Tokens) {
        self.input += other.input;
        self.output += other.output;
        self.cache_read += other.cache_read;
        self.cache_write += other.cache_write;
    }
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct ModelShare {
    pub name: String,
    pub share: f64,
}

#[derive(Default, Serialize, Clone, Debug)]
pub struct Summary {
    pub today: Tokens,
    pub cost_today: f64,
    pub cost_week: f64,
    pub cost_month: f64,
    pub burn_per_minute: u64,
    /// Share of today's API-price cost, largest first.
    pub models_today: Vec<ModelShare>,
}

struct Entry {
    at: DateTime<Utc>,
    model: String,
    tokens: Tokens,
    cost: f64,
}

#[derive(Default)]
pub struct Ledger {
    offsets: HashMap<PathBuf, u64>,
    // Claude Code writes one line per content block, each repeating the same usage.
    seen: HashSet<String>,
    entries: Vec<Entry>,
}

#[derive(Deserialize)]
struct Record {
    timestamp: Option<String>,
    #[serde(rename = "requestId")]
    request_id: Option<String>,
    message: Option<Message>,
}

#[derive(Deserialize)]
struct Message {
    id: Option<String>,
    model: Option<String>,
    usage: Option<Usage>,
}

#[derive(Deserialize)]
struct Usage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
    cache_creation: Option<CacheCreation>,
    speed: Option<String>,
}

#[derive(Deserialize)]
struct CacheCreation {
    #[serde(default)]
    ephemeral_5m_input_tokens: u64,
    #[serde(default)]
    ephemeral_1h_input_tokens: u64,
}

impl Ledger {
    /// Reads whatever was appended to logs touched since `cutoff`, and forgets entries older than it.
    pub fn tail(&mut self, projects_dir: &Path, cutoff: DateTime<Utc>) {
        let since = SystemTime::from(cutoff);
        let mut files = Vec::new();
        collect_jsonl(projects_dir, &mut files);
        for path in files {
            let touched = fs::metadata(&path).and_then(|meta| meta.modified());
            if !matches!(touched, Ok(time) if time >= since) {
                continue;
            }
            if let Ok(text) = self.read_new_lines(&path) {
                self.ingest(&text);
            }
        }
        self.entries.retain(|entry| entry.at >= cutoff);
    }

    pub fn ingest(&mut self, text: &str) {
        for line in text.lines().filter(|line| line.contains("\"usage\"")) {
            if let Some(entry) = self.parse(line) {
                self.entries.push(entry);
            }
        }
    }

    pub fn summary<Tz: TimeZone>(&self, now: &DateTime<Tz>) -> Summary {
        let (today, week, month) = period_starts(now);
        let burn_from = now.with_timezone(&Utc) - Duration::minutes(BURN_WINDOW_MINUTES);
        let mut summary = Summary::default();
        let mut cost_by_model: HashMap<&str, f64> = HashMap::new();
        let mut burn_tokens = 0;

        for entry in &self.entries {
            if entry.at >= month {
                summary.cost_month += entry.cost;
            }
            if entry.at >= week {
                summary.cost_week += entry.cost;
            }
            if entry.at >= today {
                summary.cost_today += entry.cost;
                summary.today.add(&entry.tokens);
                *cost_by_model.entry(entry.model.as_str()).or_default() += entry.cost;
            }
            if entry.at >= burn_from {
                burn_tokens += entry.tokens.total();
            }
        }

        summary.burn_per_minute = burn_tokens / BURN_WINDOW_MINUTES as u64;
        summary.models_today = model_shares(cost_by_model, summary.cost_today);
        summary
    }

    fn read_new_lines(&mut self, path: &Path) -> std::io::Result<String> {
        let offset = self.offsets.entry(path.to_path_buf()).or_insert(0);
        let mut file = File::open(path)?;
        let len = file.metadata()?.len();
        if len < *offset {
            *offset = 0;
        }
        file.seek(SeekFrom::Start(*offset))?;
        let mut bytes = Vec::new();
        file.take(len - *offset).read_to_end(&mut bytes)?;
        // A line still being written has no newline yet; leave it for the next pass.
        let complete = bytes.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
        *offset += complete as u64;
        Ok(String::from_utf8_lossy(&bytes[..complete]).into_owned())
    }

    fn parse(&mut self, line: &str) -> Option<Entry> {
        let record: Record = serde_json::from_str(line).ok()?;
        let message = record.message?;
        let usage = message.usage?;
        if let Some(id) = &message.id {
            let key = format!("{id}:{}", record.request_id.as_deref().unwrap_or_default());
            if !self.seen.insert(key) {
                return None;
            }
        }
        let at = DateTime::parse_from_rfc3339(record.timestamp.as_deref()?).ok()?.with_timezone(&Utc);
        let model = message.model.unwrap_or_default();
        let (write_5m, write_1h) = match &usage.cache_creation {
            Some(split) => (split.ephemeral_5m_input_tokens, split.ephemeral_1h_input_tokens),
            None => (usage.cache_creation_input_tokens, 0),
        };
        let cost = pricing::cost(
            &model,
            &Billable {
                input: usage.input_tokens,
                output: usage.output_tokens,
                cache_read: usage.cache_read_input_tokens,
                cache_write_5m: write_5m,
                cache_write_1h: write_1h,
                fast: usage.speed.as_deref() == Some("fast"),
            },
        );
        let tokens = Tokens {
            input: usage.input_tokens,
            output: usage.output_tokens,
            cache_read: usage.cache_read_input_tokens,
            cache_write: write_5m + write_1h,
        };
        Some(Entry { at, model, tokens, cost })
    }
}

/// Earliest moment any number on screen needs: the start of this week or this month.
pub fn scan_cutoff<Tz: TimeZone>(now: &DateTime<Tz>) -> DateTime<Utc> {
    let (_, week, month) = period_starts(now);
    week.min(month)
}

/// Starts of today, this week (Monday) and this month, on `now`'s clock.
fn period_starts<Tz: TimeZone>(now: &DateTime<Tz>) -> (DateTime<Utc>, DateTime<Utc>, DateTime<Utc>) {
    let date = now.date_naive();
    let monday = date - Duration::days(date.weekday().num_days_from_monday() as i64);
    let first_of_month = date - Duration::days(date.day0() as i64);
    let tz = now.timezone();
    (midnight(&tz, date), midnight(&tz, monday), midnight(&tz, first_of_month))
}

fn midnight<Tz: TimeZone>(tz: &Tz, date: NaiveDate) -> DateTime<Utc> {
    let local = date.and_time(NaiveTime::MIN);
    tz.from_local_datetime(&local)
        .earliest()
        .map(|time| time.with_timezone(&Utc))
        .unwrap_or_else(|| Utc.from_utc_datetime(&local))
}

fn model_shares(cost_by_model: HashMap<&str, f64>, total: f64) -> Vec<ModelShare> {
    if total <= 0.0 {
        return Vec::new();
    }
    let mut shares: Vec<ModelShare> = cost_by_model
        .into_iter()
        .filter(|(_, cost)| *cost > 0.0)
        .map(|(model, cost)| ModelShare { name: display_name(model), share: cost / total })
        .collect();
    shares.sort_by(|a, b| b.share.total_cmp(&a.share));
    shares
}

/// "claude-opus-5-5" -> "Opus 5.5"
fn display_name(model: &str) -> String {
    let mut parts = model.trim_start_matches("claude-").split('-');
    let family = parts.next().unwrap_or_default();
    let mut name: String = family.chars().take(1).flat_map(char::to_uppercase).chain(family.chars().skip(1)).collect();
    let version: Vec<&str> = parts.collect();
    if !version.is_empty() {
        name.push(' ');
        name.push_str(&version.join("."));
    }
    name
}

fn collect_jsonl(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_jsonl(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "jsonl") {
            out.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::FixedOffset;

    fn line(at: &str, id: &str, model: &str, usage: &str) -> String {
        format!(
            r#"{{"type":"assistant","timestamp":"{at}","requestId":"req-{id}","message":{{"id":"{id}","model":"{model}","usage":{usage}}}}}"#
        )
    }

    fn assert_close(actual: f64, expected: f64) {
        assert!((actual - expected).abs() < 1e-9, "expected {expected}, got {actual}");
    }

    // Wednesday 11 Nov 2026, 12:00 at UTC+1.
    fn now() -> DateTime<FixedOffset> {
        DateTime::parse_from_rfc3339("2026-11-11T12:00:00+01:00").unwrap()
    }

    fn sample_ledger() -> Ledger {
        let today_opus = line(
            "2026-11-11T09:00:00Z",
            "m1",
            "claude-opus-5-5",
            r#"{"input_tokens":1000,"output_tokens":2000,"cache_read_input_tokens":1000000,"cache_creation_input_tokens":100000,"cache_creation":{"ephemeral_5m_input_tokens":0,"ephemeral_1h_input_tokens":100000}}"#,
        );
        let lines = [
            today_opus.clone(),
            today_opus,
            line("2026-11-11T10:00:00Z", "m2", "claude-sonnet-5-5", r#"{"output_tokens":1000000}"#),
            line("2026-11-11T10:55:00Z", "m3", "claude-opus-5-5", r#"{"output_tokens":15000}"#),
            line("2026-11-10T09:00:00Z", "m4", "claude-opus-5-5", r#"{"input_tokens":1000000}"#),
            // Monday 00:30 local, still Sunday in UTC.
            line("2026-11-08T23:30:00Z", "m5", "claude-opus-5-5", r#"{"input_tokens":250000}"#),
            // Sunday 23:30 local: last week, this month.
            line("2026-11-08T22:30:00Z", "m6", "claude-opus-5-5", r#"{"input_tokens":500000}"#),
            // 31 Oct: neither this week nor this month.
            line("2026-10-31T22:00:00Z", "m7", "claude-opus-5-5", r#"{"input_tokens":1000000}"#),
            r#"{"type":"user","message":{"role":"user","content":"hi"}}"#.to_string(),
        ];
        let mut ledger = Ledger::default();
        ledger.ingest(&lines.join("\n"));
        ledger
    }

    #[test]
    fn totals_today_counting_each_reply_once() {
        let summary = sample_ledger().summary(&now());
        assert_eq!(
            summary.today,
            Tokens { input: 1000, output: 1_017_000, cache_read: 1_000_000, cache_write: 100_000 }
        );
        // Opus 1.044 + Sonnet 10 + Opus 0.3
        assert_close(summary.cost_today, 11.344);
    }

    #[test]
    fn week_starts_monday_and_month_on_the_first_in_local_time() {
        let summary = sample_ledger().summary(&now());
        assert_close(summary.cost_week, 11.344 + 4.0 + 1.0);
        assert_close(summary.cost_month, 11.344 + 4.0 + 1.0 + 2.0);
    }

    #[test]
    fn burn_rate_covers_the_last_fifteen_minutes() {
        assert_eq!(sample_ledger().summary(&now()).burn_per_minute, 1000);
    }

    #[test]
    fn models_today_are_ranked_by_cost_share() {
        let names: Vec<String> = sample_ledger().summary(&now()).models_today.into_iter().map(|m| m.name).collect();
        assert_eq!(names, ["Sonnet 5.5", "Opus 5.5"]);
    }

    #[test]
    fn tail_reads_only_appended_complete_lines() {
        let dir = std::env::temp_dir().join(format!("claude-usage-test-{}", std::process::id()));
        let nested = dir.join("project").join("session").join("subagents");
        fs::create_dir_all(&nested).unwrap();
        let file = nested.join("agent.jsonl");
        let first = line("2026-11-11T09:00:00Z", "a1", "claude-opus-5-5", r#"{"input_tokens":1000000}"#);
        let second = line("2026-11-11T09:01:00Z", "a2", "claude-opus-5-5", r#"{"input_tokens":1000000}"#);
        fs::write(&file, format!("{first}\n{}", &second[..20])).unwrap();

        let cutoff = DateTime::parse_from_rfc3339("2026-11-01T00:00:00Z").unwrap().with_timezone(&Utc);
        let mut ledger = Ledger::default();
        ledger.tail(&dir, cutoff);
        assert_close(ledger.summary(&now()).cost_today, 4.0);

        fs::write(&file, format!("{first}\n{second}\n")).unwrap();
        ledger.tail(&dir, cutoff);
        assert_close(ledger.summary(&now()).cost_today, 8.0);
        fs::remove_dir_all(&dir).unwrap();
    }
}

//! The cache notices Pi's own transcript shows, derived from the same facts.
//! Pi computes a miss only inside its TUI, so an RPC client gets raw usage and
//! has to reproduce the comparison with the previous request: a port of
//! dist/core/cache-stats.js (`detectMiss`, `scan`) and the formatters in
//! interactive-mode.js and cache-warmer.js.
use serde_json::Value;
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
};
use zeron_proto::{AgentEvent, NoticeTone};

/// Idle gaps past Anthropic's default cache lifetime are named in the notice.
const CACHE_TTL_MS: i64 = 5 * 60 * 1000;
/// Misses this small are cache breakpoint granularity, not waste.
const NOISE_FLOOR_TOKENS: u64 = 1024;
/// A miss is worth a line only past either bar.
const NOTICE_MIN_TOKENS: u64 = 20_000;
const NOTICE_MIN_COST: f64 = 0.1;
/// How much session history the seed scan reads before giving up.
const SEED_SCAN_BYTES: u64 = 16 * 1024 * 1024;

/// The request a cache entry could have served.
#[derive(Debug, Clone, PartialEq)]
struct Request {
    prompt_tokens: u64,
    model: String,
    at_ms: i64,
    /// Some request since the last reset used the provider's cache, so a
    /// later zero-cache request is a miss and not a provider without caching.
    reported_cache: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct Miss {
    tokens: u64,
    cost: f64,
    idle_ms: i64,
    model_changed: bool,
}

/// Follows the previous request across turns so the next one can be judged.
#[derive(Default)]
pub(super) struct Tracker {
    prev: Option<Request>,
}

impl Tracker {
    /// The context legitimately changed: the next prompt is new content, not
    /// re-billed content.
    pub fn reset(&mut self) {
        self.prev = None;
    }

    /// Records a completed assistant message and returns the miss it paid
    /// for. `cache_read_per_token` prices a total miss, which has no cache
    /// reads of its own to price from.
    pub fn assistant(&mut self, message: &Value, cache_read_per_token: f64) -> Option<Miss> {
        let miss = self
            .prev
            .as_ref()
            .and_then(|prev| miss(prev, message, cache_read_per_token));
        let reported = self.prev.as_ref().is_some_and(|prev| prev.reported_cache);
        self.prev = request(message, reported).or_else(|| self.prev.take());
        miss
    }

    /// Follows a persisted session entry and returns the notice Pi shows for it.
    pub fn entry(&mut self, entry: &Value) -> Option<AgentEvent> {
        match Kind::of(entry)? {
            Kind::Reset => {
                self.reset();
                compaction_notice(entry["type"].as_str()?, &entry["usage"])
            }
            Kind::Refresh => {
                let usage = &entry["usage"];
                let prompt_tokens =
                    n(usage, "input") + n(usage, "cacheRead") + n(usage, "cacheWrite");
                if prompt_tokens > 0 {
                    self.prev = Some(Request {
                        prompt_tokens,
                        model: model_key(&entry["provider"], &entry["model"]),
                        at_ms: entry["timestamp"].as_str().and_then(epoch_ms).unwrap_or(0),
                        reported_cache: true,
                    });
                }
                Some(notice(NoticeTone::Dim, refresh_text(entry)))
            }
            Kind::Assistant => {
                self.assistant(&entry["message"], 0.0);
                None
            }
        }
    }

    /// The tracker a session file leaves behind: Pi's own scan, run from the
    /// end of the file back to the first entry that settles the state.
    pub fn from_session_file(path: &Path) -> Self {
        let Ok(file) = File::open(path) else {
            return Self::default();
        };
        let mut settled = vec![];
        for line in LinesRev::new(file, SEED_SCAN_BYTES) {
            let Ok(entry) = serde_json::from_slice::<Value>(&line) else {
                continue;
            };
            let Some(kind) = Kind::of(&entry) else {
                continue;
            };
            if matches!(kind, Kind::Reset) {
                break;
            }
            let usage = if matches!(kind, Kind::Refresh) {
                &entry["usage"]
            } else {
                &entry["message"]["usage"]
            };
            let caching =
                matches!(kind, Kind::Refresh) || n(usage, "cacheRead") + n(usage, "cacheWrite") > 0;
            settled.push(entry);
            if caching {
                break;
            }
        }
        let mut tracker = Self::default();
        for entry in settled.iter().rev() {
            tracker.entry(entry);
        }
        tracker
    }
}

impl Miss {
    pub fn notice(&self) -> Option<AgentEvent> {
        if self.tokens < NOTICE_MIN_TOKENS && self.cost < NOTICE_MIN_COST {
            return None;
        }
        let label = if self.model_changed {
            "Cache miss after model switch".to_owned()
        } else if self.idle_ms >= CACHE_TTL_MS {
            format!(
                "Cache miss after {}m idle",
                (self.idle_ms as f64 / 60_000.0).round()
            )
        } else {
            "Cache miss".to_owned()
        };
        Some(notice(
            NoticeTone::Warning,
            format!(
                "{label}: {} tokens re-billed{}",
                tokens(self.tokens),
                dollars(self.cost)
            ),
        ))
    }
}

/// A finished compaction's summary request, billed on top of the turn.
pub(super) fn compaction_notice(kind: &str, usage: &Value) -> Option<AgentEvent> {
    if !usage.is_object() {
        return None;
    }
    let billed =
        n(usage, "input") + n(usage, "output") + n(usage, "cacheRead") + n(usage, "cacheWrite");
    let label = if kind == "compaction" {
        "Compaction"
    } else {
        "Branch summary"
    };
    Some(notice(
        NoticeTone::Warning,
        format!(
            "{label}: {} tokens billed{}",
            tokens(billed),
            dollars(f(&usage["cost"], "total"))
        ),
    ))
}

/// The entries that move the comparison point.
enum Kind {
    /// A compaction or branch summary: the context changed under the cache.
    Reset,
    /// A refresh Pi paid for to keep the cache alive.
    Refresh,
    Assistant,
}

impl Kind {
    fn of(entry: &Value) -> Option<Self> {
        match entry["type"].as_str()? {
            "compaction" | "branch_summary" => Some(Self::Reset),
            "usage" if entry["kind"] == "cache_warm" => Some(Self::Refresh),
            "message" if entry["message"]["role"] == "assistant" => Some(Self::Assistant),
            _ => None,
        }
    }
}

fn notice(tone: NoticeTone, text: String) -> AgentEvent {
    AgentEvent::Notice { tone, text }
}

fn n(value: &Value, key: &str) -> u64 {
    value[key].as_u64().unwrap_or(0)
}

fn f(value: &Value, key: &str) -> f64 {
    value[key].as_f64().unwrap_or(0.0)
}

pub(super) fn model_key(provider: &Value, model: &Value) -> String {
    format!(
        "{}/{}",
        provider.as_str().unwrap_or(""),
        model.as_str().unwrap_or("")
    )
}

/// Pi's `asPreviousRequest`: none when the message billed no prompt, in which
/// case the previous request still stands.
fn request(message: &Value, reported_before: bool) -> Option<Request> {
    let usage = &message["usage"];
    let prompt_tokens = n(usage, "input") + n(usage, "cacheRead") + n(usage, "cacheWrite");
    (prompt_tokens > 0).then(|| Request {
        prompt_tokens,
        model: model_key(&message["provider"], &message["model"]),
        at_ms: message["timestamp"].as_i64().unwrap_or(0),
        reported_cache: reported_before || n(usage, "cacheRead") + n(usage, "cacheWrite") > 0,
    })
}

/// Pi's `detectMiss`: prompt tokens that the previous request had cached and
/// this one paid full price for.
fn miss(prev: &Request, message: &Value, cache_read_per_token: f64) -> Option<Miss> {
    let usage = &message["usage"];
    let (input, read, write) = (
        n(usage, "input"),
        n(usage, "cacheRead"),
        n(usage, "cacheWrite"),
    );
    let prompt_tokens = input + read + write;
    if prompt_tokens == 0 || (read + write == 0 && !prev.reported_cache) {
        return None;
    }
    let tokens = prev.prompt_tokens.min(prompt_tokens).saturating_sub(read);
    if tokens <= NOISE_FLOOR_TOKENS {
        return None;
    }
    let cost = &usage["cost"];
    let paid = input + write;
    let paid_per_token = if paid > 0 {
        (f(cost, "input") + f(cost, "cacheWrite")) / paid as f64
    } else {
        0.0
    };
    let read_per_token = if read > 0 {
        f(cost, "cacheRead") / read as f64
    } else {
        cache_read_per_token
    };
    let model = model_key(&message["provider"], &message["model"]);
    Some(Miss {
        tokens,
        cost: tokens as f64 * (paid_per_token - read_per_token).max(0.0),
        idle_ms: (message["timestamp"].as_i64().unwrap_or(0) - prev.at_ms).max(0),
        model_changed: model != prev.model,
    })
}

fn dollars(cost: f64) -> String {
    if cost >= 0.01 {
        format!(" (~${cost:.2})")
    } else {
        String::new()
    }
}

/// Pi's `formatTokens`.
fn tokens(count: u64) -> String {
    let c = count as f64;
    match count {
        0..=999 => count.to_string(),
        1_000..=9_999 => format!("{:.1}k", c / 1e3),
        10_000..=999_999 => format!("{}k", (c / 1e3).round()),
        1_000_000..=9_999_999 => format!("{:.1}M", c / 1e6),
        _ => format!("{}M", (c / 1e6).round()),
    }
}

/// Pi's `formatCacheWarmingUsage`: the cost keeps at least three decimals.
fn refresh_text(entry: &Value) -> String {
    let note = entry["note"]
        .as_str()
        .map(|n| format!(" ({n})"))
        .unwrap_or_default();
    let mut cost = format!("{:.6}", f(&entry["usage"]["cost"], "total"));
    while cost.ends_with('0') && cost.len() - cost.find('.').unwrap_or(0) > 4 {
        cost.pop();
    }
    format!("Cache warmed{note}: ${cost}")
}

/// `YYYY-MM-DDTHH:MM:SS[.mmm]Z` as epoch milliseconds, the only shape Pi writes.
fn epoch_ms(iso: &str) -> Option<i64> {
    let (date, time) = iso.strip_suffix('Z')?.split_once('T')?;
    let mut d = date.split('-').map(|p| p.parse::<i64>().ok());
    let (year, month, day) = (d.next()??, d.next()??, d.next()??);
    let (clock, millis) = time.split_once('.').unwrap_or((time, "0"));
    let mut t = clock.split(':').map(|p| p.parse::<i64>().ok());
    let (hour, minute, second) = (t.next()??, t.next()??, t.next()??);
    let millis: i64 = format!("{millis:0<3}")[..3].parse().ok()?;
    // Days since 1970-01-01 for the proleptic Gregorian calendar.
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(((days * 24 + hour) * 60 + minute) * 60_000 + second * 1000 + millis)
}

/// The lines of a file from the last to the first, reading at most `budget`
/// bytes back from the end in chunks.
struct LinesRev {
    file: File,
    pos: u64,
    budget: u64,
    partial: Vec<u8>,
    lines: Vec<Vec<u8>>,
}

impl LinesRev {
    const CHUNK: u64 = 64 * 1024;

    fn new(file: File, budget: u64) -> Self {
        let pos = file.metadata().map(|m| m.len()).unwrap_or(0);
        Self {
            file,
            pos,
            budget,
            partial: vec![],
            lines: vec![],
        }
    }
}

impl Iterator for LinesRev {
    type Item = Vec<u8>;

    fn next(&mut self) -> Option<Vec<u8>> {
        loop {
            if let Some(line) = self.lines.pop() {
                return Some(line);
            }
            if self.pos == 0 {
                return (!self.partial.is_empty()).then(|| std::mem::take(&mut self.partial));
            }
            let take = self.pos.min(Self::CHUNK).min(self.budget);
            if take == 0 {
                return None;
            }
            self.pos -= take;
            self.budget -= take;
            let mut chunk = vec![0; take as usize];
            self.file.seek(SeekFrom::Start(self.pos)).ok()?;
            self.file.read_exact(&mut chunk).ok()?;
            chunk.append(&mut self.partial);
            let mut pieces = chunk.split(|b| *b == b'\n').map(<[u8]>::to_vec);
            self.partial = pieces.next().unwrap_or_default();
            self.lines = pieces.filter(|l| !l.is_empty()).collect();
        }
    }
}

#[cfg(test)]
mod tests {
    //! Ways it can fail:
    //! - A cache miss is invisible, or its tokens/cost/label differ from Pi's.
    //! - A model switch is labelled as an idle gap, or the reverse.
    //! - A warm cache, a first turn, a request right after a compaction, or a
    //!   miss under either bar is reported.
    //! - A provider that never reports caching is reported as missing it.
    //! - A refresh Pi paid for is not remembered as the previous request.
    //! - Seeding from a session file reads the wrong entry (an older request, a
    //!   request before a compaction) or trips on a line longer than a chunk.
    use super::*;
    use serde_json::json;

    const MIN: i64 = 60_000;

    fn assistant(
        provider: &str,
        model: &str,
        at_ms: i64,
        input: u64,
        read: u64,
        write: u64,
    ) -> Value {
        let rate = |tokens: u64, per_m: f64| tokens as f64 * per_m / 1e6;
        json!({
            "role": "assistant", "provider": provider, "model": model, "timestamp": at_ms,
            "usage": {
                "input": input, "output": 50, "cacheRead": read, "cacheWrite": write,
                "cost": {
                    "input": rate(input, 3.0), "output": 0.0,
                    "cacheRead": rate(read, 0.3), "cacheWrite": rate(write, 3.75),
                    "total": 0.0
                }
            }
        })
    }

    fn text(event: Option<AgentEvent>) -> Option<(NoticeTone, String)> {
        match event? {
            AgentEvent::Notice { tone, text } => Some((tone, text)),
            other => panic!("not a notice: {other:?}"),
        }
    }

    const READ_RATE: f64 = 0.3 / 1e6;

    #[test]
    fn a_total_miss_after_twelve_idle_minutes_is_priced_and_labelled_like_pi() {
        let mut tracker = Tracker::default();
        let t0 = 1_700_000_000_000;
        assert_eq!(
            tracker.assistant(&assistant("p", "m", t0, 100, 80_000, 0), READ_RATE),
            None
        );
        let miss = tracker
            .assistant(&assistant("p", "m", t0 + 12 * MIN, 80_100, 0, 0), READ_RATE)
            .expect("miss");
        assert_eq!(miss.tokens, 80_100);
        assert!((miss.cost - 0.21627).abs() < 1e-9, "{}", miss.cost);
        assert_eq!(
            text(miss.notice()),
            Some((
                NoticeTone::Warning,
                "Cache miss after 12m idle: 80k tokens re-billed (~$0.22)".into()
            ))
        );
    }

    #[test]
    fn a_model_switch_outranks_the_idle_gap_in_the_label() {
        let mut tracker = Tracker::default();
        let t0 = 1_700_000_000_000;
        tracker.assistant(&assistant("p", "m1", t0, 100, 80_000, 0), READ_RATE);
        let miss = tracker
            .assistant(
                &assistant("p", "m2", t0 + 20 * MIN, 0, 0, 80_100),
                READ_RATE,
            )
            .expect("miss");
        assert_eq!(
            text(miss.notice()).unwrap().1,
            "Cache miss after model switch: 80k tokens re-billed (~$0.28)"
        );
    }

    #[test]
    fn a_short_gap_is_a_plain_miss_and_small_misses_stay_silent() {
        let mut tracker = Tracker::default();
        let t0 = 1_700_000_000_000;
        tracker.assistant(&assistant("p", "m", t0, 100, 80_000, 0), READ_RATE);
        let miss = tracker
            .assistant(&assistant("p", "m", t0 + MIN, 40_000, 40_100, 0), READ_RATE)
            .expect("miss");
        assert_eq!(
            text(miss.notice()).unwrap().1,
            "Cache miss: 40k tokens re-billed (~$0.11)"
        );
        // 15k tokens at ~$0.04 is under both bars.
        let small = tracker
            .assistant(
                &assistant("p", "m", t0 + 2 * MIN, 15_100, 65_000, 0),
                READ_RATE,
            )
            .expect("counted");
        assert_eq!(small.tokens, 15_100);
        assert_eq!(small.notice(), None);
        // Under the noise floor it is not a miss at all.
        assert_eq!(
            tracker.assistant(
                &assistant("p", "m", t0 + 3 * MIN, 1_000, 79_100, 0),
                READ_RATE
            ),
            None
        );
    }

    #[test]
    fn hits_first_turns_resets_and_providers_without_caching_are_not_misses() {
        let t0 = 1_700_000_000_000;
        let mut tracker = Tracker::default();
        assert_eq!(
            tracker.assistant(&assistant("p", "m", t0, 80_000, 0, 0), READ_RATE),
            None
        );
        // The provider never reported caching: zero cache is not a miss.
        assert_eq!(
            tracker.assistant(&assistant("p", "m", t0 + MIN, 80_100, 0, 0), READ_RATE),
            None
        );

        let mut tracker = Tracker::default();
        tracker.assistant(&assistant("p", "m", t0, 100, 80_000, 0), READ_RATE);
        assert_eq!(
            tracker.assistant(&assistant("p", "m", t0 + MIN, 200, 80_100, 0), READ_RATE),
            None
        );
        tracker.reset();
        assert_eq!(
            tracker.assistant(&assistant("p", "m", t0 + 2 * MIN, 90_000, 0, 0), READ_RATE),
            None
        );
    }

    #[test]
    fn a_refresh_becomes_the_previous_request_and_reports_its_own_cost() {
        let t0 = 1_700_000_000_000;
        let mut tracker = Tracker::default();
        tracker.assistant(
            &assistant("p", "m", t0 - 30 * MIN, 100, 80_000, 0),
            READ_RATE,
        );
        let warm = |at: &str, note: Option<&str>, read_cost: f64| {
            let mut entry = json!({
                "type": "usage", "kind": "cache_warm", "provider": "p", "model": "m", "timestamp": at,
                "usage": {"input": 0, "output": 1, "cacheRead": 80_100, "cacheWrite": 0,
                    "cost": {"total": read_cost}}
            });
            if let Some(note) = note {
                entry["note"] = note.into();
            }
            entry
        };
        assert_eq!(
            text(tracker.entry(&warm("2023-11-14T22:13:20.000Z", None, 0.024045))),
            Some((NoticeTone::Dim, "Cache warmed: $0.024045".into()))
        );
        assert_eq!(
            text(tracker.entry(&warm(
                "2023-11-14T22:13:20.000Z",
                Some("extension override"),
                0.0123
            ))),
            Some((
                NoticeTone::Dim,
                "Cache warmed (extension override): $0.0123".into()
            ))
        );
        assert_eq!(
            text(tracker.entry(&warm("2023-11-14T22:13:20.000Z", None, 0.15))),
            Some((NoticeTone::Dim, "Cache warmed: $0.150".into()))
        );
        // 2023-11-14T22:13:20Z is 1_700_000_000 s. Idle counts from the refresh, not from t0.
        let miss = tracker
            .assistant(&assistant("p", "m", t0 + 7 * MIN, 80_100, 0, 0), READ_RATE)
            .expect("miss");
        assert_eq!(
            text(miss.notice()).unwrap().1,
            "Cache miss after 7m idle: 80k tokens re-billed (~$0.22)"
        );
    }

    #[test]
    fn compactions_reset_the_comparison_and_report_what_they_billed() {
        let mut tracker = Tracker::default();
        let t0 = 1_700_000_000_000;
        tracker.assistant(&assistant("p", "m", t0, 100, 80_000, 0), READ_RATE);
        let entry = json!({"type": "compaction", "usage": {
            "input": 30_000, "output": 1_200, "cacheRead": 0, "cacheWrite": 0, "cost": {"total": 0.0936}}});
        assert_eq!(
            text(tracker.entry(&entry)),
            Some((
                NoticeTone::Warning,
                "Compaction: 31k tokens billed (~$0.09)".into()
            ))
        );
        assert_eq!(
            tracker.assistant(&assistant("p", "m", t0 + MIN, 20_000, 0, 0), READ_RATE),
            None
        );
        assert_eq!(
            text(compaction_notice(
                "branch_summary",
                &json!({"input": 900, "output": 100, "cacheRead": 0, "cacheWrite": 0, "cost": {"total": 0.004}})
            )),
            Some((
                NoticeTone::Warning,
                "Branch summary: 1.0k tokens billed".into()
            ))
        );
        assert_eq!(compaction_notice("compaction", &Value::Null), None);
    }

    fn session_file(dir: &Path, lines: &[Value]) -> std::path::PathBuf {
        let path = dir.join("session.jsonl");
        let body: Vec<String> = lines.iter().map(Value::to_string).collect();
        std::fs::write(&path, body.join("\n") + "\n").unwrap();
        path
    }

    fn message(m: Value) -> Value {
        json!({"type": "message", "message": m})
    }

    #[test]
    fn seeding_from_a_session_file_judges_the_next_request_like_a_live_run() {
        let dir = tempfile::tempdir().unwrap();
        let t0 = 1_700_000_000_000;
        let huge = "x".repeat(300_000);
        let path = session_file(
            dir.path(),
            &[
                json!({"type": "session", "id": "s"}),
                message(assistant("p", "m", t0 - 99 * MIN, 100, 70_000, 0)),
                message(json!({"role": "toolResult", "content": huge})),
                message(assistant("p", "m", t0, 80_000, 0, 0)),
                message(json!({"role": "user", "content": "go on"})),
            ],
        );
        // The latest request had no cache hits but an earlier one did, so the
        // provider caches; and the comparison is the latest request, not that one.
        let mut tracker = Tracker::from_session_file(&path);
        let miss = tracker
            .assistant(&assistant("p", "m", t0 + 9 * MIN, 80_000, 0, 0), READ_RATE)
            .expect("miss");
        assert_eq!((miss.tokens, miss.idle_ms), (80_000, 9 * MIN));
        let mut tracker = Tracker::from_session_file(&path);
        assert_eq!(
            tracker.assistant(
                &assistant("p", "m", t0 + 9 * MIN, 100, 79_900, 0),
                READ_RATE
            ),
            None
        );
    }

    #[test]
    fn seeding_stops_at_a_compaction_and_ignores_requests_before_it() {
        let dir = tempfile::tempdir().unwrap();
        let t0 = 1_700_000_000_000;
        let path = session_file(
            dir.path(),
            &[
                json!({"type": "session", "id": "s"}),
                message(assistant("p", "m", t0, 100, 90_000, 0)),
                json!({"type": "compaction", "summary": "..."}),
                message(json!({"role": "user", "content": "next"})),
            ],
        );
        let mut tracker = Tracker::from_session_file(&path);
        assert_eq!(
            tracker.assistant(&assistant("p", "m", t0 + MIN, 20_000, 0, 0), READ_RATE),
            None,
            "the request before the compaction must not be the comparison"
        );
        assert_eq!(
            Tracker::from_session_file(&dir.path().join("missing.jsonl")).prev,
            None
        );
    }
}

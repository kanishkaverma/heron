//! Pi's footer usage line (`components/footer.js`), which RPC mode never
//! emits, as a quiet notice under each finished turn.
use serde_json::Value;
use zeron_proto::{AgentEvent, NoticeTone};

use super::cache::tokens;

#[derive(Debug, Default, Clone, Copy, PartialEq)]
struct Totals {
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
    cost: f64,
}

impl Totals {
    /// `get_session_stats` data: `{tokens: {input, ...}, cost}`.
    fn from_stats(data: &Value) -> Option<Self> {
        let tokens = data.get("tokens").filter(|t| t.is_object())?;
        Some(Self {
            cost: data["cost"].as_f64().unwrap_or(0.0),
            ..Self::from_tokens(tokens)
        })
    }
    /// An assistant message's `usage`: `{input, ..., cost: {total, ...}}`.
    fn from_usage(usage: &Value) -> Self {
        Self {
            cost: usage["cost"]["total"].as_f64().unwrap_or(0.0),
            ..Self::from_tokens(usage)
        }
    }
    fn from_tokens(value: &Value) -> Self {
        let n = |key: &str| value[key].as_u64().unwrap_or(0);
        Self {
            input: n("input"),
            output: n("output"),
            cache_read: n("cacheRead"),
            cache_write: n("cacheWrite"),
            cost: 0.0,
        }
    }
    fn add(&mut self, other: &Self) {
        self.input += other.input;
        self.output += other.output;
        self.cache_read += other.cache_read;
        self.cache_write += other.cache_write;
        self.cost += other.cost;
    }
    fn prompt(&self) -> u64 {
        self.input + self.cache_read + self.cache_write
    }
}

#[derive(Default)]
pub(super) struct Usage {
    turn: Totals,
    /// Pi's last `get_session_stats`, plus messages it has not counted yet:
    /// a turn can finish before the stats requested at its settle arrive.
    session: Option<Totals>,
    unstated: Totals,
}

impl Usage {
    pub fn stats(&mut self, data: &Value) {
        if let Some(totals) = Totals::from_stats(data) {
            self.session = Some(totals);
            self.unstated = Totals::default();
        }
    }
    pub fn assistant(&mut self, message: &Value) {
        if message["usage"].is_object() {
            let totals = Totals::from_usage(&message["usage"]);
            self.turn.add(&totals);
            self.unstated.add(&totals);
        }
    }
    /// The finished turn's line; the next turn counts from zero.
    pub fn take_notice(&mut self) -> Option<AgentEvent> {
        let turn = std::mem::take(&mut self.turn);
        let session = self.session.map(|mut session| {
            session.add(&self.unstated);
            session.cost
        });
        Some(AgentEvent::Notice {
            tone: NoticeTone::Dim,
            text: line(&turn, session)?,
        })
    }
}

/// Pi's footer stats (`↑ ↓ R W CH $`) for one turn, plus the session cost.
/// Free, cacheless turns (local models) stay quiet.
fn line(turn: &Totals, session_cost: Option<f64>) -> Option<String> {
    if turn.cost == 0.0 && turn.cache_read + turn.cache_write == 0 {
        return None;
    }
    let mut parts = vec![format!("↑{} ↓{}", tokens(turn.input), tokens(turn.output))];
    if turn.cache_read > 0 {
        parts.push(format!("R{}", tokens(turn.cache_read)));
    }
    if turn.cache_write > 0 {
        parts.push(format!("W{}", tokens(turn.cache_write)));
    }
    if turn.cache_read + turn.cache_write > 0 {
        parts.push(format!(
            "cache {:.0}%",
            turn.cache_read as f64 * 100.0 / turn.prompt() as f64
        ));
    }
    parts.push(match session_cost {
        Some(session) => format!("${:.3} turn · ${session:.3} session", turn.cost),
        None => format!("${:.3} turn", turn.cost),
    });
    Some(parts.join(" · "))
}

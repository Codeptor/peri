#![allow(dead_code)]

//! Daily digest — one message and one markdown section per UTC day.
//!
//! Fired from the day-roll in `main.rs` for the day that just ENDED, and back-filled on boot
//! when the daemon was down at midnight. Two sinks, one composition:
//!   * Telegram, as a rich HTML message (`DailyDigest::to_html`);
//!   * `docs/ledger/YYYY-MM.md`, as a `## YYYY-MM-DD` section (`DailyDigest::to_markdown`).
//!
//! **The markdown file is the delivery ledger.** A day whose section already exists is skipped
//! entirely — no second Telegram push, no duplicated section — which is what makes the roll
//! path and the boot back-fill safe to both run. That is deliberate: an idempotency marker the
//! operator can read is worth more than one hidden in the meta table.
//!
//! Everything above the I/O is pure. [`compose`] takes seeded rows and returns the whole
//! digest, so the numbers an operator will read are pinned by tests without a clock or a disk.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use tracing::{debug, info, warn};

use crate::contracts::Trade;
use crate::ledger::Store;
use crate::notify::{Notify, esc, row};

/// Where the markdown ledger lives, relative to the daemon's working directory
/// (`%h/botta/kestreld` under systemd) — i.e. the repo's own `docs/ledger`.
pub const LEDGER_DIR_DEFAULT: &str = "../docs/ledger";

/// The suffix `Store::append_decision_reason` writes for a risk-gate refusal. Refusals are
/// never pushed to Telegram individually (dozens a day); they are counted here once.
pub const GATE_REFUSED_TOKEN: &str = "gate_refused:";

const DAY_MS: i64 = 86_400_000;

/// Veto counterfactual totals, carried only when at least one has been replayed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CfSummary {
    pub computed: i64,
    pub net_actual: f64,
    pub net_bracket: f64,
}

/// One UTC day, as the operator reads it.
#[derive(Debug, Clone, PartialEq)]
pub struct DailyDigest {
    pub day: String,
    pub equity_open: f64,
    pub equity_close: f64,
    pub equity_pct: f64,
    /// `SUM(realized_pnl) - SUM(fee)` over the day's trades — the same identity
    /// `Store::equity` uses, windowed to the day.
    pub net_pnl: f64,
    pub fees: f64,
    pub exits: usize,
    pub tp: usize,
    pub sl: usize,
    pub veto: usize,
    pub other: usize,
    pub wins: usize,
    pub win_rate: f64,
    pub top: Option<(String, f64)>,
    pub bottom: Option<(String, f64)>,
    /// Gate refusals by kind, most frequent first (ties broken alphabetically).
    pub refusals: Vec<(String, usize)>,
    pub counterfactual: Option<CfSummary>,
}

/// UTC day key (`YYYY-MM-DD`) for an epoch-millis stamp.
pub fn day_key(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|d| d.format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

/// Count ` gate_refused:<kind>` suffixes across decision reasons. A kind token runs to the
/// next whitespace — the reason string is a space-joined bag of `key:value` tokens, and a
/// single decision can carry more than one refusal suffix over its life.
pub fn parse_gate_refusals(reasons: &[String]) -> Vec<(String, usize)> {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for r in reasons {
        for (idx, _) in r.match_indices(GATE_REFUSED_TOKEN) {
            let rest = &r[idx + GATE_REFUSED_TOKEN.len()..];
            let kind = rest.split_whitespace().next().unwrap_or("");
            if !kind.is_empty() {
                *counts.entry(kind).or_insert(0) += 1;
            }
        }
    }
    let mut out: Vec<(String, usize)> = counts.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    out
}

/// Sum that returns `+0.0` for an empty set. `Iterator::sum::<f64>()` seeds with `-0.0` (the
/// IEEE additive identity), which would render a flat day as an alarming `net pnl  -0.00`.
fn sum_f64(vals: impl Iterator<Item = f64>) -> f64 {
    vals.fold(0.0, |acc, v| acc + v)
}

/// Net per market over a set of trades: realized minus every fee, opens included. The sums of
/// these are exactly `DailyDigest::net_pnl`, so top/bottom always reconcile with the total.
fn net_by_market(trades: &[Trade]) -> Vec<(String, f64)> {
    let mut by: HashMap<&str, f64> = HashMap::new();
    for t in trades {
        *by.entry(t.market.as_str()).or_insert(0.0) += t.realized_pnl - t.fee;
    }
    let mut out: Vec<(String, f64)> = by.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
    // net desc, market asc — deterministic under ties so the digest never flaps.
    out.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal).then_with(|| a.0.cmp(&b.0)));
    out
}

/// Build the whole digest from seeded rows. Pure — no clock, no ledger, no disk.
///
/// * `trades` — every trade stamped inside the day (opens included: their fee was charged then).
/// * `reasons` — `decisions.reason` for the day, mined for gate refusals.
/// * `counterfactual` — `Store::counterfactual_totals`, dropped when nothing has been replayed.
pub fn compose(
    day: &str,
    equity_open: f64,
    equity_close: f64,
    trades: &[Trade],
    reasons: &[String],
    counterfactual: Option<(i64, f64, f64)>,
) -> DailyDigest {
    let equity_pct = if equity_open.abs() > 1e-9 {
        (equity_close - equity_open) / equity_open * 100.0
    } else {
        0.0
    };
    let net_pnl = sum_f64(trades.iter().map(|t| t.realized_pnl - t.fee));
    let fees = sum_f64(trades.iter().map(|t| t.fee));

    let closes: Vec<&Trade> = trades.iter().filter(|t| t.action != "open").collect();
    let count = |a: &str| closes.iter().filter(|t| t.action == a).count();
    let tp = count("tp");
    let sl = count("sl");
    let veto = count("veto_close");
    let exits = closes.len();
    // `other` is defined as the remainder so the four buckets always sum to the day's exits,
    // whatever a future loop names its close reason (today: `time_stop`).
    let other = exits - tp - sl - veto;
    // A win is a close that made money AFTER its own exit fee — at 7.5bp a side, a scratch
    // exit is a loss, and calling it a win is how a losing day reads as a 60% win rate.
    let wins = closes.iter().filter(|t| t.realized_pnl - t.fee > 0.0).count();
    let win_rate = if exits > 0 { wins as f64 / exits as f64 * 100.0 } else { 0.0 };

    let ranked = net_by_market(trades);
    let top = ranked.first().cloned();
    let bottom = ranked.last().cloned();

    DailyDigest {
        day: day.to_string(),
        equity_open,
        equity_close,
        equity_pct,
        net_pnl,
        fees,
        exits,
        tp,
        sl,
        veto,
        other,
        wins,
        win_rate,
        top,
        bottom,
        refusals: parse_gate_refusals(reasons),
        counterfactual: counterfactual
            .filter(|(computed, _, _)| *computed > 0)
            .map(|(computed, net_actual, net_bracket)| CfSummary { computed, net_actual, net_bracket }),
    }
}

impl DailyDigest {
    /// No equity, no exits, no refusals: the daemon did not exist that day. That is the
    /// "yesterday" of a fresh install, and reporting `$0.00 → $0.00` for it would be a lie
    /// dressed as a digest — so the boot back-fill skips it and leaves the day unwritten.
    ///
    /// A QUIET day is NOT blank: an equity curve exists (bankroll is never 0.00), so "we
    /// took nothing today" still gets reported, which is exactly what an operator needs.
    pub fn is_blank(&self) -> bool {
        self.equity_open == 0.0 && self.equity_close == 0.0 && self.exits == 0 && self.refusals.is_empty()
    }

    /// The body, as `(label, value)` pairs. Both renderings read from here, so the Telegram
    /// message and the markdown section can never disagree about a number.
    fn rows(&self) -> Vec<(&'static str, String)> {
        let money = |m: &Option<(String, f64)>| match m {
            Some((market, net)) => format!("{market} {net:+.2}"),
            None => "none".to_string(),
        };
        let mut rows = vec![
            (
                "equity",
                format!(
                    "${:.2} → ${:.2}  ({:+.2}%)",
                    self.equity_open, self.equity_close, self.equity_pct
                ),
            ),
            ("net pnl", format!("{:+.2}", self.net_pnl)),
            ("fees", format!("{:.2}", self.fees)),
            (
                "exits",
                format!("{}  tp {} · sl {} · veto {} · other {}", self.exits, self.tp, self.sl, self.veto, self.other),
            ),
            ("win rate", format!("{:.0}%  ({}/{})", self.win_rate, self.wins, self.exits)),
            ("top", money(&self.top)),
            ("bottom", money(&self.bottom)),
        ];
        let refused = if self.refusals.is_empty() {
            "none".to_string()
        } else {
            self.refusals.iter().map(|(k, n)| format!("{k} {n}")).collect::<Vec<_>>().join(" · ")
        };
        rows.push(("refused", refused));
        if let Some(cf) = self.counterfactual {
            rows.push((
                "veto cf",
                format!("actual {:+.2} vs bracket {:+.2} ({} replayed)", cf.net_actual, cf.net_bracket, cf.computed),
            ));
        }
        rows
    }

    /// Aligned monospace body shared by both renderings.
    ///
    /// HTML-escaped even on the markdown path so the two sinks stay byte-identical below the
    /// heading. That is free in practice: every value is a number, a `CamelCase` refusal kind,
    /// or a market name (`BTC`, `xyz:TSLA`) — none of which contain `& < >`.
    fn body(&self) -> String {
        self.rows().iter().map(|(l, v)| row(l, v)).collect::<Vec<_>>().join("\n")
    }

    /// Telegram HTML: emoji header plus one `<pre>` block, which is the only Telegram element
    /// that actually aligns columns.
    pub fn to_html(&self) -> String {
        format!("📊 <b>Daily digest — {}</b>\n<pre>{}</pre>", esc(&self.day), self.body())
    }

    /// Markdown section for `docs/ledger/YYYY-MM.md`. Same body, fenced, under an `## day`
    /// heading — the heading is what [`has_section`] looks for.
    pub fn to_markdown(&self) -> String {
        format!("## {}\n\n```\n{}\n```\n", self.day, self.body())
    }
}

/// Read the whole day from the ledger and compose it.
///
/// Equity open prefers the `day_open:` stamp the day-roll writes (the same number the kill
/// switch measures against); it falls back to the first equity sample of the day. Close
/// prefers the last sample of the day, then the NEXT day's open stamp — which the roll sets
/// to the equity at midnight, i.e. exactly this day's close.
pub async fn collect(store: &Store, day_start_ms: i64) -> DailyDigest {
    let day = day_key(day_start_ms);
    let next_day = day_key(day_start_ms + DAY_MS);
    let day_end = day_start_ms + DAY_MS;

    let trades = store.trades_between(day_start_ms, day_end).await.unwrap_or_default();
    let reasons = store.decision_reasons_between(day_start_ms, day_end).await.unwrap_or_default();
    let (first, last) = store.equity_bounds(day_start_ms, day_end).await.unwrap_or((None, None));
    let stamp_open = store.day_open_equity(&day).await.unwrap_or(None);
    let stamp_close = store.day_open_equity(&next_day).await.unwrap_or(None);
    let equity_open = stamp_open.or(first).unwrap_or(0.0);
    let equity_close = last.or(stamp_close).unwrap_or(equity_open);
    let cf = store.counterfactual_totals().await.ok();

    compose(&day, equity_open, equity_close, &trades, &reasons, cf)
}

/// `dir/YYYY-MM.md` for a day key. `None` for a malformed key — the caller skips the file
/// sink rather than writing `.md` into the directory root.
pub fn month_file(dir: &Path, day: &str) -> Option<PathBuf> {
    day.get(..7).map(|month| dir.join(format!("{month}.md")))
}

/// Has this day already been written? Matches the section heading exactly, so a day mentioned
/// inside prose can never be mistaken for a delivered digest.
pub fn has_section(existing: &str, day: &str) -> bool {
    let header = format!("## {day}");
    existing.lines().any(|l| l.trim_end() == header)
}

/// The whole idempotency decision: send-and-append only when the month file does not already
/// carry this day's section. `None` = no file yet (first day of the month, or first run).
pub fn needs_digest(existing: Option<&str>, day: &str) -> bool {
    !existing.map(|e| has_section(e, day)).unwrap_or(false)
}

/// Append one section to `dir/YYYY-MM.md`, creating the directory and the file's month
/// heading on first write. `Ok(false)` = the section was already there and nothing was
/// written. Blocking I/O on purpose: a few kilobytes, once a day.
pub fn append_markdown(dir: &Path, day: &str, section: &str) -> std::io::Result<bool> {
    let (Some(path), Some(month)) = (month_file(dir, day), day.get(..7)) else {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, format!("bad day key {day}")));
    };
    std::fs::create_dir_all(dir)?;
    let existing = std::fs::read_to_string(&path).ok();
    if !needs_digest(existing.as_deref(), day) {
        return Ok(false);
    }
    let mut out = match existing {
        Some(e) => {
            let mut e = e;
            if !e.ends_with('\n') {
                e.push('\n');
            }
            e.push('\n');
            e
        }
        None => format!("# Ledger — {month}\n\n"),
    };
    out.push_str(section);
    std::fs::write(&path, out)?;
    Ok(true)
}

/// Deliver one day's digest: skip if already delivered, else Telegram + markdown append.
/// Best-effort end to end — a failed push or a read-only docs directory costs visibility,
/// never the trading loop.
pub async fn deliver(store: &Store, notify: &Notify, dir: &Path, day_start_ms: i64) {
    let day = day_key(day_start_ms);
    let existing = month_file(dir, &day).and_then(|p| std::fs::read_to_string(p).ok());
    if !needs_digest(existing.as_deref(), &day) {
        debug!(day = %day, "daily digest already delivered — skipping");
        return;
    }
    let digest = collect(store, day_start_ms).await;
    if digest.is_blank() {
        debug!(day = %day, "no ledger evidence for that day — nothing to digest");
        return;
    }
    info!(
        day = %digest.day,
        net_pnl = digest.net_pnl,
        exits = digest.exits,
        equity_pct = digest.equity_pct,
        "daily digest"
    );
    notify.send(&digest.to_html()).await;
    match append_markdown(dir, &day, &digest.to_markdown()) {
        Ok(true) => info!(day = %day, dir = %dir.display(), "daily digest appended to ledger"),
        Ok(false) => debug!(day = %day, "ledger section already present"),
        Err(e) => warn!(day = %day, error = %e, "ledger append failed — digest was still sent"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY_START: i64 = 1_786_147_200_000; // 2026-08-08T00:00:00Z

    fn trade(market: &str, action: &str, realized: f64, fee: f64) -> Trade {
        Trade {
            id: 0,
            position_id: 0,
            market: market.into(),
            action: action.into(),
            px: 100.0,
            size: 1.0,
            fee,
            realized_pnl: realized,
            ts: DAY_START,
            fill_mode: "flat".into(),
        }
    }

    fn seeded() -> DailyDigest {
        // per-market net (opens included): SOL +14.05 · BTC -9.25 · ETH +0.70 => +5.50 total
        let trades = vec![
            trade("SOL", "open", 0.0, 0.15),
            trade("SOL", "tp", 14.5, 0.30),   // exit net +14.20
            trade("BTC", "open", 0.0, 0.15),
            trade("BTC", "sl", -8.95, 0.15),  // exit net  -9.10
            trade("ETH", "open", 0.0, 0.10),
            trade("ETH", "veto_close", 0.10, 0.10), // exit net 0.00 -> not a win
            trade("ETH", "time_stop", 1.00, 0.20),  // exit net +0.80
        ];
        let reasons = vec![
            "model muse refused:false latency:900 gate_refused:Cooldown".to_string(),
            "model muse refused:false latency:800 gate_refused:Cooldown".to_string(),
            "model muse refused:false latency:700 gate_refused:PerMarketCap".to_string(),
            "model muse refused:false latency:600".to_string(),
        ];
        compose("2026-08-08", 1000.0, 1005.50, &trades, &reasons, Some((3, -1.0, 2.0)))
    }

    #[test]
    fn compose_reconciles_pnl_fees_mix_and_win_rate() {
        let d = seeded();
        assert_eq!(d.day, "2026-08-08");
        assert!((d.net_pnl - 5.50).abs() < 1e-9, "net {}", d.net_pnl);
        assert!((d.fees - 1.15).abs() < 1e-9, "fees {}", d.fees);
        assert!((d.equity_pct - 0.55).abs() < 1e-9, "pct {}", d.equity_pct);
        // exit mix: opens never count, and the buckets sum to the exits
        assert_eq!((d.exits, d.tp, d.sl, d.veto, d.other), (4, 1, 1, 1, 1));
        assert_eq!(d.tp + d.sl + d.veto + d.other, d.exits);
        // a scratch veto (net exactly 0.00) is NOT a win
        assert_eq!(d.wins, 2, "tp and time_stop only");
        assert!((d.win_rate - 50.0).abs() < 1e-9);
        // top/bottom are per-market nets INCLUDING the day's entry fees, so they sum to net_pnl
        assert_eq!(d.top.as_ref().unwrap().0, "SOL");
        assert!((d.top.as_ref().unwrap().1 - 14.05).abs() < 1e-9, "top {:?}", d.top);
        assert_eq!(d.bottom.as_ref().unwrap().0, "BTC");
        assert!((d.bottom.as_ref().unwrap().1 - -9.25).abs() < 1e-9, "bottom {:?}", d.bottom);
        let ranked_sum: f64 = net_by_market(&[
            trade("SOL", "open", 0.0, 0.15),
            trade("SOL", "tp", 14.5, 0.30),
            trade("BTC", "open", 0.0, 0.15),
            trade("BTC", "sl", -8.95, 0.15),
            trade("ETH", "open", 0.0, 0.10),
            trade("ETH", "veto_close", 0.10, 0.10),
            trade("ETH", "time_stop", 1.00, 0.20),
        ])
        .iter()
        .map(|(_, n)| n)
        .sum();
        assert!((ranked_sum - d.net_pnl).abs() < 1e-9, "per-market nets must reconcile with the total");
        assert_eq!(d.refusals, vec![("Cooldown".to_string(), 2), ("PerMarketCap".to_string(), 1)]);
        assert_eq!(d.counterfactual, Some(CfSummary { computed: 3, net_actual: -1.0, net_bracket: 2.0 }));
    }

    #[test]
    fn compose_on_a_silent_day_renders_none_not_blank() {
        let d = compose("2026-08-08", 1000.0, 1000.0, &[], &[], Some((0, 0.0, 0.0)));
        assert_eq!((d.net_pnl, d.fees, d.exits, d.wins), (0.0, 0.0, 0, 0));
        assert_eq!(d.win_rate, 0.0, "no exits must not divide by zero");
        assert!(d.top.is_none() && d.bottom.is_none());
        assert!(d.refusals.is_empty());
        assert_eq!(d.counterfactual, None, "nothing replayed yet -> the row is dropped");
        let html = d.to_html();
        assert!(html.contains("top      none") && html.contains("refused  none"), "{html}");
        assert!(!html.contains("veto cf"), "{html}");
        // a flat day must not render the f64 `Sum` identity as a scary -0.00
        assert!(d.to_html().contains("net pnl  +0.00"), "{}", d.to_html());
        assert!(d.to_html().contains("fees     0.00"), "{}", d.to_html());
        // a zero equity open cannot produce NaN%
        let zero = compose("2026-08-08", 0.0, 0.0, &[], &[], None);
        assert_eq!(zero.equity_pct, 0.0);
        // and a day with NO ledger evidence at all is never delivered
        assert!(zero.is_blank(), "a day the daemon did not exist for is blank");
        assert!(!d.is_blank(), "a quiet day with an equity curve is still reported");
        assert!(!seeded().is_blank());
    }

    #[test]
    fn html_snapshot_is_pinned_and_escaped() {
        let d = seeded();
        assert_eq!(
            d.to_html(),
            "📊 <b>Daily digest — 2026-08-08</b>\n<pre>equity   $1000.00 → $1005.50  (+0.55%)\n\
             net pnl  +5.50\nfees     1.15\nexits    4  tp 1 · sl 1 · veto 1 · other 1\n\
             win rate 50%  (2/4)\ntop      SOL +14.05\nbottom   BTC -9.25\n\
             refused  Cooldown 2 · PerMarketCap 1\nveto cf  actual -1.00 vs bracket +2.00 (3 replayed)</pre>"
        );
        // market names reach the message from the venue: they are escaped like everything else
        let hostile = vec![trade("A<B&C", "tp", 1.0, 0.1)];
        let h = compose("2026-08-08", 1.0, 1.0, &hostile, &[], None).to_html();
        assert!(h.contains("A&lt;B&amp;C"), "{h}");
        assert!(!h.contains("A<B&C"));
    }

    #[test]
    fn markdown_and_html_carry_the_same_numbers() {
        let d = seeded();
        let md = d.to_markdown();
        assert!(md.starts_with("## 2026-08-08\n\n```\n"), "{md}");
        assert!(md.ends_with("```\n"), "{md}");
        for line in d.body().lines() {
            assert!(md.contains(line), "markdown missing body line {line}");
        }
        assert!(has_section(&md, "2026-08-08"));
    }

    #[test]
    fn refusal_parsing_counts_kinds_and_ignores_prose() {
        assert!(parse_gate_refusals(&[]).is_empty());
        let reasons = vec![
            "no refusal here".to_string(),
            // the word alone is not a refusal — only the token with its kind
            "gate_refused mentioned in prose".to_string(),
            "a gate_refused:StaleData b".to_string(),
            "two in one row gate_refused:StaleData gate_refused:Regime".to_string(),
            "trailing gate_refused:Paced".to_string(),
        ];
        assert_eq!(
            parse_gate_refusals(&reasons),
            vec![
                ("StaleData".to_string(), 2),
                ("Paced".to_string(), 1),
                ("Regime".to_string(), 1),
            ],
            "count desc, then alphabetical"
        );
        // a dangling token with no kind is not counted
        assert!(parse_gate_refusals(&["x gate_refused: ".to_string()]).is_empty());
    }

    #[test]
    fn section_detection_is_exact() {
        let file = "# Ledger — 2026-08\n\n## 2026-08-08\n\n```\nequity\n```\n";
        assert!(has_section(file, "2026-08-08"));
        assert!(!has_section(file, "2026-08-09"), "another day is not this day");
        assert!(!has_section("mentions 2026-08-08 in prose", "2026-08-08"), "prose is not a section");
        assert!(!has_section("### 2026-08-08", "2026-08-08"), "a deeper heading is not the section");
        assert!(has_section("## 2026-08-08   ", "2026-08-08"), "trailing space tolerated");
        // the backfill decision reads exactly this
        assert!(needs_digest(None, "2026-08-08"), "no file at all -> deliver");
        assert!(!needs_digest(Some(file), "2026-08-08"), "already delivered -> skip");
        assert!(needs_digest(Some(file), "2026-08-09"), "missed roll -> back-fill on boot");
    }

    #[test]
    fn append_is_idempotent_and_creates_the_month_file() {
        let dir = std::env::temp_dir().join(format!(
            "kestreld-digest-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        let d = seeded();
        assert_eq!(month_file(&dir, "2026-08-08"), Some(dir.join("2026-08.md")));

        assert!(append_markdown(&dir, "2026-08-08", &d.to_markdown()).expect("first append"));
        let after_first = std::fs::read_to_string(dir.join("2026-08.md")).unwrap();
        assert!(after_first.starts_with("# Ledger — 2026-08\n"), "{after_first}");
        assert_eq!(after_first.matches("## 2026-08-08").count(), 1);

        // same day again: nothing written, byte-identical file
        assert!(!append_markdown(&dir, "2026-08-08", &d.to_markdown()).expect("second append"));
        assert_eq!(std::fs::read_to_string(dir.join("2026-08.md")).unwrap(), after_first);

        // next day appends a second section to the same month file
        let mut next = d.clone();
        next.day = "2026-08-09".into();
        assert!(append_markdown(&dir, "2026-08-09", &next.to_markdown()).expect("next day"));
        let after_second = std::fs::read_to_string(dir.join("2026-08.md")).unwrap();
        assert_eq!(after_second.matches("## 2026-08-08").count(), 1);
        assert_eq!(after_second.matches("## 2026-08-09").count(), 1);
        assert!(after_second.contains("```\n\n## 2026-08-09"), "sections are blank-line separated:\n{after_second}");
        // a new month opens a new file
        let mut sep = d.clone();
        sep.day = "2026-09-01".into();
        assert!(append_markdown(&dir, "2026-09-01", &sep.to_markdown()).expect("new month"));
        assert!(dir.join("2026-09.md").exists());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn day_keys_and_month_paths_are_derived_from_the_stamp() {
        assert_eq!(day_key(DAY_START), "2026-08-08");
        assert_eq!(day_key(DAY_START + DAY_MS - 1), "2026-08-08", "last ms of the day");
        assert_eq!(day_key(DAY_START + DAY_MS), "2026-08-09");
        assert_eq!(month_file(Path::new("/x"), "2026-08-08"), Some(PathBuf::from("/x/2026-08.md")));
        assert_eq!(month_file(Path::new("/x"), "bad"), None, "a malformed key has no month file");
        assert!(append_markdown(Path::new("/x"), "bad", "s").is_err());
    }

    #[tokio::test]
    async fn collect_reads_the_day_from_a_seeded_ledger() {
        use crate::contracts::Side;
        use crate::sizing::Sized;

        let store = Store::open("sqlite::memory:").await.expect("open memory");
        let now = chrono::Utc::now().timestamp_millis();
        let day_start = crate::risk::utc_day_start_ms(now);
        let day = day_key(day_start);

        // day-open stamp + a live equity curve
        store.set_day_open_equity(&day, 1000.0).await.unwrap();
        store.snapshot_equity(day_start + 1000, 1000.0).await.unwrap();
        store.snapshot_equity(now, 1004.0).await.unwrap();

        let sized = Sized { leverage: 10.0, margin: 20.0, notional: 200.0, stop_pct: 1.0, tp_pct: 2.0 };
        let win = store.open_position("SOL", Side::Long, &sized, 100.0, false, 24.0, None).await.unwrap();
        store.close_position(win.id, 103.0, "tp", None).await.unwrap();
        let loss = store.open_position("BTC", Side::Long, &sized, 100.0, false, 24.0, None).await.unwrap();
        store.close_position(loss.id, 99.0, "sl", None).await.unwrap();
        let id = store
            .log_decision(now, "ETH", "open", "long", 0.81, "t", 24.0, false, false, "model muse refused:false")
            .await
            .unwrap();
        store.append_decision_reason(id, " gate_refused:Cooldown").await.unwrap();

        let d = collect(&store, day_start).await;
        assert_eq!(d.day, day);
        assert!((d.equity_open - 1000.0).abs() < 1e-9, "day-open stamp wins over the first sample");
        assert!((d.equity_close - 1004.0).abs() < 1e-9, "close is the last sample of the day");
        assert_eq!((d.exits, d.tp, d.sl), (2, 1, 1));
        assert_eq!(d.wins, 1);
        assert_eq!(d.top.as_ref().unwrap().0, "SOL");
        assert_eq!(d.bottom.as_ref().unwrap().0, "BTC");
        assert_eq!(d.refusals, vec![("Cooldown".to_string(), 1)]);
        assert_eq!(d.counterfactual, None, "nothing veto-closed -> no counterfactual row");
        // net reconciles with the ledger's own identity for the window
        let sum: (f64,) = sqlx::query_as("SELECT SUM(realized_pnl) - SUM(fee) FROM trades")
            .fetch_one(store.pool())
            .await
            .unwrap();
        assert!((d.net_pnl - sum.0).abs() < 1e-9);

        // a day the daemon never ran: no samples, no trades — open/close fall back to the stamps
        let quiet = day_start - DAY_MS;
        store.set_day_open_equity(&day_key(quiet), 990.0).await.unwrap();
        let q = collect(&store, quiet).await;
        assert!((q.equity_open - 990.0).abs() < 1e-9);
        assert!((q.equity_close - 1000.0).abs() < 1e-9, "the NEXT day's open stamp is this day's close");
        assert_eq!(q.exits, 0);
    }

    #[tokio::test]
    async fn deliver_is_idempotent_across_a_roll_and_a_boot_backfill() {
        let store = Store::open("sqlite::memory:").await.expect("open memory");
        // unconfigured notifier: log-only, still exercises the whole path
        let notify = Notify::new("UNSET_TOKEN_ENV_XYZ".into(), String::new());
        let dir = std::env::temp_dir().join(format!(
            "kestreld-deliver-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        let day_start = crate::risk::utc_day_start_ms(chrono::Utc::now().timestamp_millis());
        // a day the daemon actually lived through — otherwise `is_blank` (correctly) skips it
        store.set_day_open_equity(&day_key(day_start), 1000.0).await.unwrap();

        deliver(&store, &notify, &dir, day_start).await;
        let path = dir.join(format!("{}.md", &day_key(day_start)[..7]));
        let first = std::fs::read_to_string(&path).expect("ledger written");
        assert_eq!(first.matches(&format!("## {}", day_key(day_start))).count(), 1);

        // the boot back-fill running after the roll must be a complete no-op
        deliver(&store, &notify, &dir, day_start).await;
        assert_eq!(std::fs::read_to_string(&path).unwrap(), first);

        std::fs::remove_dir_all(&dir).ok();
    }
}

//! `/api/analytics` — decision quality, measured from the ledger (Phase T2).
//!
//! Three read-only aggregations (conviction vs outcome, per-market record, exit mix by day)
//! plus the one piece of real computation in the daemon's read path: **veto counterfactuals**.
//! Every time the reviewer closes a position early we lose the only evidence that would say
//! whether it was right — the bracket never got to resolve. Replaying the position's own
//! SL/TP against 1m candles recovers it, and because the answer can never change once the
//! window has passed, it is computed at most once per position and cached forever.

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::backtest::fills;
use crate::contracts::Side;
use crate::hl_rest::{Candle, HlRest};
use crate::ledger::{Store, VetoClose};

/// The walker's verdict. Defined with the walk rules in [`crate::backtest::fills`] (spec
/// Decision 5: one fill model, shared by this endpoint and the replay engine) and re-exported
/// here, where it is the `counterfactuals.bracket_outcome` domain.
pub use crate::backtest::fills::BracketOutcome;

/// Conviction bucket labels, low to high. Emitted in full every request (zero rows included)
/// so the dashboard table never reflows as buckets fill.
pub const CONVICTION_BUCKETS: [&str; 3] = ["0.70-0.75", "0.75-0.80", "0.80+"];

/// How long after a decision row its position may open and still count as the same trade.
/// The open follows `log_decision` in the same task within seconds; the window only has to
/// absorb a slow ledger write, and `DupMarket` guarantees no second position on the market
/// in the meantime.
pub const DECISION_MATCH_WINDOW_MS: i64 = 600_000;

/// Exit-mix history depth: today plus the previous 13 UTC days.
pub const EXIT_MIX_DAYS: i64 = 14;

/// Taker fee charged on the replayed exit — identical to the paper ledger's 7.5bp, so a
/// bracket pnl and an actual pnl are the same kind of number.
pub const EXIT_FEE_RATE: f64 = fills::FEE_RATE;

/// Longest candle window a counterfactual will ever fetch, whatever the position's horizon.
pub const CF_MAX_WINDOW_MS: i64 = 24 * 60 * 60 * 1000;

/// Per-request work bound: uncached positions replayed on one `/api/analytics` call. The rest
/// are reported as `pending` and picked up by the next request.
pub const CF_MAX_PER_REQUEST: i64 = 3;

/// How many per-position counterfactual rows the payload carries, newest close first. The
/// totals stay whole-history; this only bounds the detail table (and the response size).
pub const CF_ROWS_MAX: i64 = 50;

/// Wall-clock budget for the whole replay phase, so a slow venue degrades the endpoint's
/// latency instead of hanging it. Each fetch is additionally bounded by what is left.
pub const CF_BUDGET: Duration = Duration::from_secs(12);

/// Candle interval the replay walks. 1m is the finest HL serves and bounds the intra-candle
/// ambiguity (see `walk_bracket`) to under a minute of price action.
pub const CF_INTERVAL: &str = "1m";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnalyticsResp {
    pub conviction_buckets: Vec<ConvictionBucket>,
    pub per_market: Vec<MarketRecord>,
    pub exit_mix_daily: Vec<ExitMixDay>,
    pub counterfactuals: CounterfactualSummary,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConvictionBucket {
    pub bucket: String,
    pub closes: i64,
    pub wins: i64,
    pub net_pnl: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MarketRecord {
    pub market: String,
    pub trades: i64,
    pub net_pnl: f64,
    pub fees: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExitMixDay {
    pub date: String,
    pub tp: i64,
    pub sl: i64,
    pub veto_close: i64,
    pub other: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CounterfactualSummary {
    pub computed: i64,
    pub pending: i64,
    pub net_actual: f64,
    pub net_bracket: f64,
    /// The `CF_ROWS_MAX` most recently closed computed counterfactuals, newest first. A window
    /// onto the same rows the totals sum — the totals stay whole-history, so with more than
    /// `CF_ROWS_MAX` replays the rows no longer add up to `net_actual` / `net_bracket`.
    pub rows: Vec<CounterfactualRow>,
}

/// One veto the replay has judged: what the reviewer's early close actually paid against what
/// the position's own bracket would have. `bracket_pnl - actual_pnl` is the cost of that veto.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CounterfactualRow {
    pub position_id: i64,
    pub market: String,
    pub side: Side,
    pub closed_ts: i64,
    pub actual_pnl: f64,
    pub bracket_pnl: f64,
    /// `tp` | `sl` | `expiry` — `BracketOutcome::as_str`.
    pub bracket_outcome: String,
}

/// Which bucket a conviction falls in, or `None` when it is below the entry floor.
///
/// 0.70 is the *historical* entry floor (`conviction_min` was user-locked there until
/// amendment #10 raised it to 0.75), so nothing under 0.70 can have been executed; the
/// 0.70-0.75 bucket stays because it holds the pre-amendment closes that motivated the
/// raise. A row below 0.70 is legacy or hand-edited and must not be folded into that bucket
/// where it would distort the very number this endpoint exists to calibrate. A non-finite
/// conviction is corruption, not data, and is dropped for the same reason — NaN in particular
/// would otherwise fall through every `<` comparison into the top bucket.
pub fn conviction_bucket(conviction: f64) -> Option<usize> {
    if !conviction.is_finite() || conviction < 0.70 {
        return None;
    }
    if conviction < 0.75 {
        Some(0)
    } else if conviction < 0.80 {
        Some(1)
    } else {
        Some(2)
    }
}

/// Bucket `(conviction, net_pnl)` pairs into the fixed three buckets. A win is strictly
/// positive net pnl — a scratch (exactly 0.0, or a fee-only loss) is not a win.
pub fn bucket_outcomes(rows: &[(f64, f64)]) -> Vec<ConvictionBucket> {
    let mut out: Vec<ConvictionBucket> = CONVICTION_BUCKETS
        .iter()
        .map(|b| ConvictionBucket { bucket: (*b).to_string(), closes: 0, wins: 0, net_pnl: 0.0 })
        .collect();
    for (conviction, net) in rows {
        let Some(i) = conviction_bucket(*conviction) else { continue };
        out[i].closes += 1;
        if *net > 0.0 {
            out[i].wins += 1;
        }
        out[i].net_pnl += *net;
    }
    out
}

/// Sort the venue's candles chronologically and walk them through the SHARED bracket rules
/// ([`fills::walk`]): stop before target inside a candle, first touch wins, an untouched
/// window expires at the final close, no candles at all is `None` (not computable — the caller
/// retries rather than caching a guess).
///
/// The rules themselves are documented on [`fills`], and the replay engine walks its tape
/// through the same function, so a counterfactual and a backtest can never disagree about
/// where a bracket got out.
pub fn walk_bracket(candles: &[Candle], side: Side, sl_px: f64, tp_px: f64) -> Option<(BracketOutcome, f64)> {
    let mut ordered: Vec<&Candle> = candles.iter().collect();
    ordered.sort_by_key(|c| c.t);
    fills::walk(ordered.into_iter().map(|c| fills::Bar::new(c.h, c.l, c.c)), side, sl_px, tp_px)
}

/// Exit-side pnl of leaving `size` at `exit_px`: gross move minus the 7.5bp taker fee on the
/// exit notional. The entry fee is deliberately absent — it was paid either way, so leaving it
/// out makes `bracket_pnl - actual_pnl` exactly the cost of the veto.
pub fn exit_pnl(side: Side, size: f64, entry_px: f64, exit_px: f64) -> f64 {
    let gross = match side {
        Side::Long => (exit_px - entry_px) * size,
        Side::Short => (entry_px - exit_px) * size,
    };
    gross - exit_px * size * EXIT_FEE_RATE
}

/// How much of the position's horizon was left when the reviewer closed it, capped at 24h.
/// Zero when the horizon had already elapsed (the time stop would have fired immediately).
pub fn remaining_horizon_ms(opened_ts: i64, horizon_hours: Option<f64>, close_ts: i64) -> i64 {
    let hours = horizon_hours.filter(|h| h.is_finite() && *h > 0.0).unwrap_or(24.0);
    let end = opened_ts.saturating_add((hours * 3_600_000.0) as i64);
    (end - close_ts).clamp(0, CF_MAX_WINDOW_MS)
}

/// Replay one veto close. `None` means "not computable right now" (no candles) — never cached,
/// retried on the next request.
async fn replay(hl: &HlRest, v: &VetoClose, budget: Duration) -> Option<(BracketOutcome, f64)> {
    let window = remaining_horizon_ms(v.opened_ts, v.horizon_hours, v.close_ts);
    if window == 0 {
        // Horizon already spent at the moment of the veto: the bracket had no life left, so it
        // expires where the position actually closed. No fetch, no retry, deterministic.
        return Some((BracketOutcome::Expiry, v.close_px));
    }
    let fetch = hl.candle_snapshot(&v.market, CF_INTERVAL, v.close_ts, v.close_ts + window);
    let candles = match tokio::time::timeout(budget, fetch).await {
        Ok(Ok(c)) => c,
        Ok(Err(e)) => {
            warn!(market=%v.market, position_id=v.position_id, error=%e, "counterfactual candles failed — retrying next request");
            return None;
        }
        Err(_) => {
            warn!(market=%v.market, position_id=v.position_id, "counterfactual candles timed out — retrying next request");
            return None;
        }
    };
    // Candles that had already CLOSED before the veto describe price action we did not live
    // through; the one straddling the close is kept (its bucket is where the veto happened).
    let live: Vec<Candle> = candles.into_iter().filter(|c| c.T > v.close_ts).collect();
    if live.is_empty() {
        warn!(market=%v.market, position_id=v.position_id, "counterfactual window returned no candles — retrying next request");
        return None;
    }
    walk_bracket(&live, v.side, v.sl_px, v.tp_px)
}

/// Replay up to `CF_MAX_PER_REQUEST` uncached veto closes, oldest first, inside `CF_BUDGET`.
/// Every success is cached before the next one starts, so a budget cut-off never loses work.
async fn compute_pending(store: &Store, hl: &HlRest, now_ms: i64) {
    let pending = match store.counterfactual_pending(CF_MAX_PER_REQUEST).await {
        Ok(rows) => rows,
        Err(e) => {
            warn!(error=%e, "counterfactual backlog read failed");
            return;
        }
    };
    let deadline = Instant::now() + CF_BUDGET;
    for v in pending {
        let Some(left) = deadline.checked_duration_since(Instant::now()) else {
            break;
        };
        let Some((outcome, exit_px)) = replay(hl, &v, left).await else {
            continue;
        };
        let bracket_pnl = exit_pnl(v.side, v.size, v.entry_px, exit_px);
        if let Err(e) = store
            .counterfactual_put(v.position_id, now_ms, outcome.as_str(), bracket_pnl, v.actual_pnl)
            .await
        {
            warn!(position_id = v.position_id, error=%e, "counterfactual cache write failed");
            continue;
        }
        info!(
            market = %v.market,
            position_id = v.position_id,
            outcome = outcome.as_str(),
            bracket_pnl,
            actual_pnl = v.actual_pnl,
            "veto counterfactual computed"
        );
    }
}

/// Assemble the whole `/api/analytics` payload. Lazily drains the counterfactual backlog first
/// so the summary it reports includes whatever this request just computed.
pub async fn build(store: &Store, hl: &HlRest, now_ms: i64) -> AnalyticsResp {
    compute_pending(store, hl, now_ms).await;

    let conviction_buckets = bucket_outcomes(
        &store.executed_decision_outcomes(DECISION_MATCH_WINDOW_MS).await.unwrap_or_default(),
    );
    let per_market = store
        .market_stats()
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|m| MarketRecord { market: m.market, trades: m.trades, net_pnl: m.net_pnl, fees: m.fees })
        .collect();
    let since = crate::risk::utc_day_start_ms(now_ms) - (EXIT_MIX_DAYS - 1) * 86_400_000;
    let exit_mix_daily = store
        .exit_mix_since(since)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|d| ExitMixDay { date: d.date, tp: d.tp, sl: d.sl, veto_close: d.veto_close, other: d.other })
        .collect();
    let (computed, net_actual, net_bracket) = store.counterfactual_totals().await.unwrap_or((0, 0.0, 0.0));
    let pending = store.counterfactual_pending_count().await.unwrap_or(0);
    let rows = store
        .counterfactual_rows(CF_ROWS_MAX)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|r| CounterfactualRow {
            position_id: r.position_id,
            market: r.market,
            side: r.side,
            closed_ts: r.closed_ts,
            actual_pnl: r.actual_pnl,
            bracket_pnl: r.bracket_pnl,
            bracket_outcome: r.bracket_outcome,
        })
        .collect();

    AnalyticsResp {
        conviction_buckets,
        per_market,
        exit_mix_daily,
        counterfactuals: CounterfactualSummary { computed, pending, net_actual, net_bracket, rows },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::Side;

    fn candle(t: i64, o: f64, h: f64, l: f64, c: f64) -> Candle {
        Candle { t, T: t + 59_999, s: "SOL".into(), i: "1m".into(), o, c, h, l, v: 1.0, n: 1 }
    }

    #[test]
    fn conviction_buckets_split_on_their_exact_boundaries() {
        // lower edge of each bucket belongs to that bucket
        assert_eq!(conviction_bucket(0.70), Some(0));
        assert_eq!(conviction_bucket(0.7499), Some(0));
        assert_eq!(conviction_bucket(0.75), Some(1), "0.75 opens the middle bucket");
        assert_eq!(conviction_bucket(0.7999), Some(1));
        assert_eq!(conviction_bucket(0.80), Some(2), "0.80 opens the top bucket");
        assert_eq!(conviction_bucket(1.0), Some(2));
        // below the user-locked entry floor: not counted anywhere
        assert_eq!(conviction_bucket(0.6999), None);
        assert_eq!(conviction_bucket(0.0), None);
        assert_eq!(conviction_bucket(-1.0), None);
        // non-finite conviction is corruption: NaN must not slip into the top bucket via
        // failed comparisons, and infinity is not a confidence level
        assert_eq!(conviction_bucket(f64::NAN), None);
        assert_eq!(conviction_bucket(f64::INFINITY), None);
        assert_eq!(conviction_bucket(f64::NEG_INFINITY), None);
    }

    #[test]
    fn bucketing_counts_closes_wins_and_net() {
        let rows = vec![
            (0.70, 1.0),    // bucket 0, win
            (0.74, -2.0),   // bucket 0, loss
            (0.75, 0.0),    // bucket 1, scratch is NOT a win
            (0.79, 3.0),    // bucket 1, win
            (0.95, -0.5),   // bucket 2, loss
            (0.10, 100.0),  // below floor: ignored entirely
        ];
        let b = bucket_outcomes(&rows);
        assert_eq!(b.len(), 3, "all three buckets always reported");
        assert_eq!(b[0].bucket, "0.70-0.75");
        assert_eq!((b[0].closes, b[0].wins), (2, 1));
        assert!((b[0].net_pnl - -1.0).abs() < 1e-9);
        assert_eq!((b[1].closes, b[1].wins), (2, 1), "0.0 net is a close but not a win");
        assert!((b[1].net_pnl - 3.0).abs() < 1e-9);
        assert_eq!((b[2].closes, b[2].wins), (1, 0));
        assert!((b[2].net_pnl - -0.5).abs() < 1e-9);
        // empty input still yields the three labelled rows
        let empty = bucket_outcomes(&[]);
        assert_eq!(empty.iter().map(|r| r.closes).sum::<i64>(), 0);
        assert_eq!(empty[2].bucket, "0.80+");
    }

    #[test]
    fn walk_takes_tp_when_it_comes_first() {
        // long entry 100, sl 99, tp 102 — candle 2 tags the tp, candle 3 would have hit the sl
        let candles = vec![
            candle(0, 100.0, 100.5, 99.5, 100.2),
            candle(60_000, 100.2, 102.4, 100.0, 102.0),
            candle(120_000, 102.0, 102.5, 98.0, 98.5),
        ];
        let (outcome, px) = walk_bracket(&candles, Side::Long, 99.0, 102.0).expect("resolved");
        assert_eq!(outcome, BracketOutcome::Tp);
        assert!((px - 102.0).abs() < 1e-9, "fills at the trigger, not the candle close");
    }

    #[test]
    fn walk_takes_sl_when_it_comes_first() {
        let candles = vec![
            candle(0, 100.0, 100.5, 99.5, 100.2),
            candle(60_000, 100.2, 100.4, 98.9, 99.0),  // sl 99.0 touched
            candle(120_000, 99.0, 103.0, 99.0, 102.9), // tp would come later — must not win
        ];
        let (outcome, px) = walk_bracket(&candles, Side::Long, 99.0, 102.0).expect("resolved");
        assert_eq!(outcome, BracketOutcome::Sl);
        assert!((px - 99.0).abs() < 1e-9);
    }

    #[test]
    fn same_candle_touching_both_is_scored_as_the_stop() {
        // one wide candle straddles sl AND tp: the path is unknown, so the stop wins
        let both = vec![candle(0, 100.0, 103.0, 98.0, 101.0)];
        let (outcome, px) = walk_bracket(&both, Side::Long, 99.0, 102.0).expect("resolved");
        assert_eq!(outcome, BracketOutcome::Sl, "ambiguous candle must never flatter the bracket");
        assert!((px - 99.0).abs() < 1e-9);
        // shorts are mirrored: sl above, tp below
        let (outcome_s, px_s) = walk_bracket(&both, Side::Short, 102.0, 99.0).expect("resolved");
        assert_eq!(outcome_s, BracketOutcome::Sl);
        assert!((px_s - 102.0).abs() < 1e-9);
    }

    #[test]
    fn walk_expires_at_the_final_close_when_nothing_is_touched() {
        let candles = vec![
            candle(0, 100.0, 100.5, 99.5, 100.2),
            candle(60_000, 100.2, 101.0, 99.8, 100.7),
        ];
        let (outcome, px) = walk_bracket(&candles, Side::Long, 99.0, 102.0).expect("resolved");
        assert_eq!(outcome, BracketOutcome::Expiry);
        assert!((px - 100.7).abs() < 1e-9, "expiry marks at the last close");
        // no candles at all is not an outcome — the caller must retry, not cache a guess
        assert!(walk_bracket(&[], Side::Long, 99.0, 102.0).is_none());
    }

    #[test]
    fn walk_is_chronological_whatever_order_the_venue_returns() {
        // same three candles as the tp test, shuffled: the tp still resolves first
        let candles = vec![
            candle(120_000, 102.0, 102.5, 98.0, 98.5),
            candle(0, 100.0, 100.5, 99.5, 100.2),
            candle(60_000, 100.2, 102.4, 100.0, 102.0),
        ];
        let (outcome, _) = walk_bracket(&candles, Side::Long, 99.0, 102.0).expect("resolved");
        assert_eq!(outcome, BracketOutcome::Tp, "out-of-order input must be sorted before walking");
    }

    #[test]
    fn short_bracket_directions_are_mirrored() {
        // short from 100: sl ABOVE at 101, tp BELOW at 98
        let up = vec![candle(0, 100.0, 101.2, 99.9, 101.0)];
        assert_eq!(walk_bracket(&up, Side::Short, 101.0, 98.0).unwrap().0, BracketOutcome::Sl);
        let down = vec![candle(0, 100.0, 100.1, 97.9, 98.0)];
        assert_eq!(walk_bracket(&down, Side::Short, 101.0, 98.0).unwrap().0, BracketOutcome::Tp);
    }

    #[test]
    fn exit_pnl_charges_seven_and_a_half_bp_on_the_exit_notional() {
        // long 2 units from 100 to 102: gross 4.0, fee 102*2*0.00075 = 0.153
        let long = exit_pnl(Side::Long, 2.0, 100.0, 102.0);
        assert!((long - (4.0 - 0.153)).abs() < 1e-9, "long tp pnl {long}");
        // long stopped at 99: gross -2.0, fee 99*2*0.00075 = 0.1485
        let stopped = exit_pnl(Side::Long, 2.0, 100.0, 99.0);
        assert!((stopped - (-2.0 - 0.1485)).abs() < 1e-9, "long sl pnl {stopped}");
        // short is the mirror: down move is profit
        let short = exit_pnl(Side::Short, 2.0, 100.0, 98.0);
        assert!((short - (4.0 - 98.0 * 2.0 * 0.00075)).abs() < 1e-9, "short tp pnl {short}");
        // a flat exit is a pure fee loss — never zero
        let flat = exit_pnl(Side::Long, 2.0, 100.0, 100.0);
        assert!(flat < 0.0 && (flat - -0.15).abs() < 1e-9, "flat exit still pays the fee: {flat}");
    }

    #[test]
    fn remaining_horizon_is_capped_and_floored() {
        let opened = 1_786_233_600_000i64;
        // closed 1h into a 24h horizon -> 23h left
        assert_eq!(remaining_horizon_ms(opened, Some(24.0), opened + 3_600_000), 23 * 3_600_000);
        // a 48h horizon is capped at 24h of candles
        assert_eq!(remaining_horizon_ms(opened, Some(48.0), opened + 3_600_000), CF_MAX_WINDOW_MS);
        // closed after the horizon expired -> nothing left to replay
        assert_eq!(remaining_horizon_ms(opened, Some(2.0), opened + 3 * 3_600_000), 0);
        // missing / nonsense horizons fall back to the 24h default
        assert_eq!(remaining_horizon_ms(opened, None, opened), 24 * 3_600_000);
        assert_eq!(remaining_horizon_ms(opened, Some(f64::NAN), opened), 24 * 3_600_000);
        assert_eq!(remaining_horizon_ms(opened, Some(0.0), opened), 24 * 3_600_000);
    }

    #[test]
    fn outcome_tokens_match_the_table_check_constraint() {
        assert_eq!(BracketOutcome::Tp.as_str(), "tp");
        assert_eq!(BracketOutcome::Sl.as_str(), "sl");
        assert_eq!(BracketOutcome::Expiry.as_str(), "expiry");
    }

    #[test]
    fn analytics_payload_round_trips() {
        let resp = AnalyticsResp {
            conviction_buckets: bucket_outcomes(&[(0.82, 1.5)]),
            per_market: vec![MarketRecord { market: "SOL".into(), trades: 2, net_pnl: -0.4, fees: 0.9 }],
            exit_mix_daily: vec![ExitMixDay { date: "2026-08-09".into(), tp: 1, sl: 2, veto_close: 1, other: 0 }],
            counterfactuals: CounterfactualSummary {
                computed: 3,
                pending: 1,
                net_actual: -1.0,
                net_bracket: 2.0,
                rows: vec![CounterfactualRow {
                    position_id: 42,
                    market: "SOL".into(),
                    side: Side::Short,
                    closed_ts: 1_786_284_000_000,
                    actual_pnl: -0.4,
                    bracket_pnl: 1.1,
                    bracket_outcome: BracketOutcome::Tp.as_str().to_string(),
                }],
            },
        };
        let json = serde_json::to_value(&resp).expect("serialize");
        assert_eq!(json["conviction_buckets"][2]["bucket"], serde_json::json!("0.80+"));
        assert_eq!(json["per_market"][0]["market"], serde_json::json!("SOL"));
        assert_eq!(json["exit_mix_daily"][0]["veto_close"], serde_json::json!(1));
        assert_eq!(json["counterfactuals"]["pending"], serde_json::json!(1));
        // per-position detail rides inside the counterfactual object, beside the totals
        let row = &json["counterfactuals"]["rows"][0];
        assert_eq!(row["position_id"], serde_json::json!(42));
        assert_eq!(row["market"], serde_json::json!("SOL"));
        assert_eq!(row["side"], serde_json::json!("short"), "side is the lowercase token the dash reads");
        assert_eq!(row["closed_ts"], serde_json::json!(1_786_284_000_000i64));
        assert_eq!(row["bracket_outcome"], serde_json::json!("tp"));
        assert_eq!(row["actual_pnl"], serde_json::json!(-0.4));
        assert_eq!(row["bracket_pnl"], serde_json::json!(1.1));
        let back: AnalyticsResp = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back, resp);
    }
}

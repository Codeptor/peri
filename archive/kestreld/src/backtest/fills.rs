//! Fill rules — ONE implementation, two consumers (spec Decision 5).
//!
//! The counterfactual walker in [`crate::analytics`] and the replay engine in
//! [`super::engine`] both have to answer the same question: given a position's bracket and a
//! tape of 1m bars, where does it get out? Two implementations of that would be two different
//! backtests, so the rules live here and `analytics::walk_bracket` is a thin adapter over
//! [`walk`] — its behaviour, and the tests that pin it, are unchanged.
//!
//! The rules:
//!
//!   * **SL before TP inside a bar.** A bar whose range touches both levels is scored as the
//!     STOP. The path inside a minute is unknown and a replay that flattered the bracket would
//!     make every veto look like a mistake (and every backtest look profitable).
//!   * **First touch wins.** Bars are walked in order and the first one that touches a level
//!     ends the position AT that level — not at the bar's close.
//!   * **Untouched through the whole window expires at the last close** — what the time stop
//!     would have done.
//!
//! On top of those, the entry side the engine needs and the walker does not: a fill at the
//! next bar's open plus flat slip (2bp native / 5bp dex), and 7.5bp of taker fee per side.

use crate::contracts::Side;

/// Taker fee, per side. The paper ledger charges the same 7.5bp on open and on close
/// (`ledger::Store::open_position` / `close_position`), so a replayed pnl and a live paper pnl
/// are the same kind of number.
pub const FEE_RATE: f64 = 0.00075;

/// Flat entry slip on a native perp — `ledger::Store::resolve_open_fill`'s no-book path.
pub const SLIP_NATIVE: f64 = 0.0002;
/// Flat entry slip on a dex (`xyz:`) market, where books are thinner.
pub const SLIP_DEX: f64 = 0.0005;

/// What a bracket did.
///
/// Lives here rather than in `analytics` because the walker that produces it lives here;
/// `analytics` re-exports it, so `counterfactuals.bracket_outcome` and its CHECK constraint
/// are untouched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BracketOutcome {
    Tp,
    Sl,
    Expiry,
}

impl BracketOutcome {
    /// Stable token — also the `counterfactuals.bracket_outcome` CHECK domain.
    pub fn as_str(&self) -> &'static str {
        match self {
            BracketOutcome::Tp => "tp",
            BracketOutcome::Sl => "sl",
            BracketOutcome::Expiry => "expiry",
        }
    }
}

/// The three prices a bracket decision needs from one bar. Deliberately not a candle: both
/// consumers have their own candle type (`hl_rest::Candle`, `backtest::cache::CachedCandle`)
/// and neither should have to convert into the other's.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bar {
    pub h: f64,
    pub l: f64,
    pub c: f64,
}

impl Bar {
    pub fn new(h: f64, l: f64, c: f64) -> Self {
        Self { h, l, c }
    }
}

/// Which level this ONE bar touches, stop first. `None` when neither is reached.
pub fn touch(bar: Bar, side: Side, sl_px: f64, tp_px: f64) -> Option<BracketOutcome> {
    let (sl_hit, tp_hit) = match side {
        Side::Long => (bar.l <= sl_px, bar.h >= tp_px),
        Side::Short => (bar.h >= sl_px, bar.l <= tp_px),
    };
    if sl_hit {
        return Some(BracketOutcome::Sl);
    }
    if tp_hit {
        return Some(BracketOutcome::Tp);
    }
    None
}

/// Walk `bars` IN THE ORDER GIVEN and return `(outcome, exit price)`.
///
/// Callers own the ordering — `analytics::walk_bracket` sorts the venue's response, the replay
/// engine walks its dense tape one minute at a time. `None` means the window held no bars at
/// all, which is not an outcome: the caller retries rather than caching a guess.
pub fn walk(
    bars: impl IntoIterator<Item = Bar>,
    side: Side,
    sl_px: f64,
    tp_px: f64,
) -> Option<(BracketOutcome, f64)> {
    let mut last_close = None;
    for bar in bars {
        match touch(bar, side, sl_px, tp_px) {
            Some(BracketOutcome::Sl) => return Some((BracketOutcome::Sl, sl_px)),
            Some(BracketOutcome::Tp) => return Some((BracketOutcome::Tp, tp_px)),
            _ => last_close = Some(bar.c),
        }
    }
    last_close.map(|px| (BracketOutcome::Expiry, px))
}

/// Flat slip for a market: dex markets (`xyz:` prefix) are the wider of the two, exactly as
/// the paper ledger splits them.
pub fn slip_for(market: &str) -> f64 {
    if market.starts_with("xyz:") { SLIP_DEX } else { SLIP_NATIVE }
}

/// Entry fill: the next bar's open, moved against us by the flat slip.
///
/// Mirrors `ledger::Store::resolve_open_fill`'s book-less path — the only path a backtest can
/// take, since historical order books are not served. Pinned against it in the tests below.
pub fn entry_fill_px(open_px: f64, side: Side, market: &str) -> f64 {
    let slip = slip_for(market);
    match side {
        Side::Long => open_px * (1.0 + slip),
        Side::Short => open_px * (1.0 - slip),
    }
}

/// `(sl_px, tp_px)` off a fill, in percent — `ledger::Store::open_position`'s bracket formula.
pub fn bracket_pxs(fill_px: f64, side: Side, stop_pct: f64, tp_pct: f64) -> (f64, f64) {
    match side {
        Side::Long => (fill_px * (1.0 - stop_pct / 100.0), fill_px * (1.0 + tp_pct / 100.0)),
        Side::Short => (fill_px * (1.0 + stop_pct / 100.0), fill_px * (1.0 - tp_pct / 100.0)),
    }
}

/// Taker fee on a notional, either side of the trade.
pub fn fee(notional: f64) -> f64 {
    notional.abs() * FEE_RATE
}

/// Gross (price-only) pnl of leaving `size` at `exit_px` — the ledger's `realized_pnl`, which
/// carries no fee. Fees are reported separately so a report can show both.
pub fn gross_pnl(side: Side, size: f64, entry_px: f64, exit_px: f64) -> f64 {
    match side {
        Side::Long => (exit_px - entry_px) * size,
        Side::Short => (entry_px - exit_px) * size,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::Store;

    fn bar(h: f64, l: f64, c: f64) -> Bar {
        Bar::new(h, l, c)
    }

    #[test]
    fn a_bar_touching_both_levels_is_scored_as_the_stop() {
        let both = bar(103.0, 98.0, 101.0);
        assert_eq!(touch(both, Side::Long, 99.0, 102.0), Some(BracketOutcome::Sl));
        // shorts mirror: sl above, tp below
        assert_eq!(touch(both, Side::Short, 102.0, 99.0), Some(BracketOutcome::Sl));
        // and the walk agrees, filling AT the stop
        let (outcome, px) = walk([both], Side::Long, 99.0, 102.0).expect("resolved");
        assert_eq!(outcome, BracketOutcome::Sl);
        assert!((px - 99.0).abs() < 1e-12, "fills at the trigger, not the close");
    }

    #[test]
    fn first_touch_wins_and_later_bars_are_never_reached() {
        // tp on bar 2; bar 3 would have stopped out — it must not be walked
        let bars = [bar(100.5, 99.5, 100.2), bar(102.4, 100.0, 102.0), bar(102.5, 98.0, 98.5)];
        assert_eq!(walk(bars, Side::Long, 99.0, 102.0), Some((BracketOutcome::Tp, 102.0)));
        // sl on bar 2; bar 3's tp must not win
        let bars = [bar(100.5, 99.5, 100.2), bar(100.4, 98.9, 99.0), bar(103.0, 99.0, 102.9)];
        assert_eq!(walk(bars, Side::Long, 99.0, 102.0), Some((BracketOutcome::Sl, 99.0)));
    }

    #[test]
    fn boundaries_are_inclusive_on_both_levels() {
        // exactly at the stop is a stop; a hair above it is not
        assert_eq!(touch(bar(100.5, 99.0, 100.0), Side::Long, 99.0, 102.0), Some(BracketOutcome::Sl));
        assert_eq!(touch(bar(100.5, 99.001, 100.0), Side::Long, 99.0, 102.0), None);
        // exactly at the target is a target
        assert_eq!(touch(bar(102.0, 100.0, 101.0), Side::Long, 99.0, 102.0), Some(BracketOutcome::Tp));
        assert_eq!(touch(bar(101.999, 100.0, 101.0), Side::Long, 99.0, 102.0), None);
        // short side, same convention
        assert_eq!(touch(bar(101.0, 100.0, 100.5), Side::Short, 101.0, 98.0), Some(BracketOutcome::Sl));
        assert_eq!(touch(bar(100.0, 98.0, 99.0), Side::Short, 101.0, 98.0), Some(BracketOutcome::Tp));
    }

    #[test]
    fn an_untouched_window_expires_at_the_last_close_and_an_empty_one_is_no_outcome() {
        let bars = [bar(100.5, 99.5, 100.2), bar(101.0, 99.8, 100.7)];
        assert_eq!(walk(bars, Side::Long, 99.0, 102.0), Some((BracketOutcome::Expiry, 100.7)));
        assert_eq!(walk([], Side::Long, 99.0, 102.0), None, "no bars is not an outcome");
    }

    /// The entry model is the paper ledger's own flat-slip path, not a second opinion about it.
    #[test]
    fn entry_slip_matches_the_paper_ledgers_bookless_fill() {
        for (market, px) in [("BTC", 60_000.0f64), ("SOL", 187.25), ("xyz:TSLA", 412.5)] {
            for side in [Side::Long, Side::Short] {
                let is_xyz = market.starts_with("xyz:");
                let ledger = Store::resolve_open_fill(px, 800.0, side, is_xyz, None);
                let ours = entry_fill_px(px, side, market);
                assert!((ours - ledger).abs() < 1e-12, "{market} {side:?}: {ours} vs ledger {ledger}");
            }
        }
        // direction: a long pays up, a short sells down
        assert!(entry_fill_px(100.0, Side::Long, "BTC") > 100.0);
        assert!(entry_fill_px(100.0, Side::Short, "BTC") < 100.0);
        // dex markets slip 5bp, natives 2bp
        assert!((entry_fill_px(100.0, Side::Long, "xyz:TSLA") - 100.05).abs() < 1e-12);
        assert!((entry_fill_px(100.0, Side::Long, "BTC") - 100.02).abs() < 1e-12);
        assert!((slip_for("xyz:TSLA") - SLIP_DEX).abs() < 1e-12);
        assert!((slip_for("BTC") - SLIP_NATIVE).abs() < 1e-12);
    }

    #[test]
    fn brackets_and_fees_are_the_ledgers_arithmetic() {
        // long from 100 with a 1% stop and a 2% target
        let (sl, tp) = bracket_pxs(100.0, Side::Long, 1.0, 2.0);
        assert!((sl - 99.0).abs() < 1e-12);
        assert!((tp - 102.0).abs() < 1e-12);
        // short is mirrored
        let (sl_s, tp_s) = bracket_pxs(100.0, Side::Short, 1.0, 2.0);
        assert!((sl_s - 101.0).abs() < 1e-12);
        assert!((tp_s - 98.0).abs() < 1e-12);
        // 7.5bp a side, same rate the ledger charges on open and close
        assert!((fee(800.0) - 0.6).abs() < 1e-12);
        assert!((fee(816.0) - 0.612).abs() < 1e-12);
        // gross pnl carries no fee, and shorts profit on the way down
        assert!((gross_pnl(Side::Long, 8.0, 100.0, 102.0) - 16.0).abs() < 1e-12);
        assert!((gross_pnl(Side::Short, 8.0, 100.0, 102.0) - -16.0).abs() < 1e-12);
        assert!((gross_pnl(Side::Short, 8.0, 100.0, 98.0) - 16.0).abs() < 1e-12);
    }

    /// A bracket taken at its trigger always pays exactly `notional × pct`, whatever the price
    /// — the identity every hand-computed expectation in the engine tests rests on. The exit
    /// FEE is not side-symmetric: a long's target sits 2% above the fill and its stop 1% below,
    /// and a short's are mirrored, so the exit notional differs by side.
    #[test]
    fn a_bracket_exit_pays_notional_times_the_bracket_percent() {
        for px in [3.5f64, 100.0, 61_234.75] {
            for (side, tp_fee, sl_fee) in
                [(Side::Long, 0.612, 0.594), (Side::Short, 0.588, 0.606)]
            {
                let fill = entry_fill_px(px, side, "BTC");
                let notional = 800.0;
                let size = notional / fill;
                let (sl, tp) = bracket_pxs(fill, side, 1.0, 2.0);
                assert!((gross_pnl(side, size, fill, tp) - 16.0).abs() < 1e-9, "tp pays 2% of notional");
                assert!((gross_pnl(side, size, fill, sl) - -8.0).abs() < 1e-9, "sl pays -1% of notional");
                assert!((fee(size * tp) - tp_fee).abs() < 1e-9, "{side:?} tp exit fee");
                assert!((fee(size * sl) - sl_fee).abs() < 1e-9, "{side:?} sl exit fee");
            }
        }
    }
}

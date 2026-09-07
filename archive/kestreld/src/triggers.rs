#![allow(dead_code)]
#![allow(unused_imports)]
#![allow(unused_mut)]

use crate::contracts::{Position, Side};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TriggerAction {
    Sl,
    Tp,
    TimeStop,
}

/// Check SL/TP/time-stop. Uses mid price and now_ms timestamp.
/// SL/TP: for long, SL hit when mid <= sl_px, TP when mid >= tp_px; short inverted.
/// TimeStop: when now_ms >= opened_ts + horizon_hours*3600*1000 (default 24h if None)
pub fn check_triggers(pos: &Position, mid: f64, now_ms: i64) -> Option<TriggerAction> {
    // SL/TP first
    match pos.side {
        Side::Long => {
            if mid <= pos.sl_px {
                return Some(TriggerAction::Sl);
            }
            if mid >= pos.tp_px {
                return Some(TriggerAction::Tp);
            }
        }
        Side::Short => {
            if mid >= pos.sl_px {
                return Some(TriggerAction::Sl);
            }
            if mid <= pos.tp_px {
                return Some(TriggerAction::Tp);
            }
        }
    }
    // time stop
    let horizon_ms = pos.horizon_hours.unwrap_or(24.0) * 3600.0 * 1000.0;
    if (now_ms as f64) >= (pos.opened_ts as f64 + horizon_ms) {
        return Some(TriggerAction::TimeStop);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::{Position, Side};

    fn pos_long() -> Position {
        Position {
            id: 1,
            market: "SOL".into(),
            side: Side::Long,
            entry_px: 100.0,
            size: 1.0,
            leverage: 10.0,
            margin: 10.0,
            sl_px: 98.0,
            tp_px: 104.0,
            opened_ts: 1_700_000_000_000,
            analyst: String::new(),
            horizon_hours: Some(24.0),
        }
    }

    fn pos_short() -> Position {
        Position {
            id: 2,
            market: "BTC".into(),
            side: Side::Short,
            entry_px: 100.0,
            size: 1.0,
            leverage: 10.0,
            margin: 10.0,
            sl_px: 102.0,
            tp_px: 96.0,
            opened_ts: 1_700_000_000_000,
            analyst: String::new(),
            horizon_hours: Some(24.0),
        }
    }

    #[test]
    fn long_sl_hit() {
        let p = pos_long();
        assert_eq!(check_triggers(&p, 98.0, p.opened_ts + 1000), Some(TriggerAction::Sl));
        assert_eq!(check_triggers(&p, 97.9, p.opened_ts + 1000), Some(TriggerAction::Sl));
        assert_eq!(check_triggers(&p, 98.1, p.opened_ts + 1000), None);
    }

    #[test]
    fn long_tp_hit() {
        let p = pos_long();
        assert_eq!(check_triggers(&p, 104.0, p.opened_ts + 1000), Some(TriggerAction::Tp));
        assert_eq!(check_triggers(&p, 104.1, p.opened_ts + 1000), Some(TriggerAction::Tp));
        assert_eq!(check_triggers(&p, 103.9, p.opened_ts + 1000), None);
    }

    #[test]
    fn short_sl_inverted() {
        let p = pos_short();
        // short SL at 102, hit when mid >=102
        assert_eq!(check_triggers(&p, 102.0, p.opened_ts + 1000), Some(TriggerAction::Sl));
        assert_eq!(check_triggers(&p, 102.1, p.opened_ts + 1000), Some(TriggerAction::Sl));
        assert_eq!(check_triggers(&p, 101.9, p.opened_ts + 1000), None);
    }

    #[test]
    fn short_tp_inverted() {
        let p = pos_short();
        assert_eq!(check_triggers(&p, 96.0, p.opened_ts + 1000), Some(TriggerAction::Tp));
        assert_eq!(check_triggers(&p, 95.9, p.opened_ts + 1000), Some(TriggerAction::Tp));
        assert_eq!(check_triggers(&p, 96.1, p.opened_ts + 1000), None);
    }

    #[test]
    fn time_stop() {
        let mut p = pos_long();
        let horizon_ms = (24.0 * 3600.0 * 1000.0) as i64;
        // just before horizon
        assert_eq!(check_triggers(&p, 100.0, p.opened_ts + horizon_ms - 1), None);
        // exactly at horizon
        assert_eq!(check_triggers(&p, 100.0, p.opened_ts + horizon_ms), Some(TriggerAction::TimeStop));
        // after
        assert_eq!(check_triggers(&p, 100.0, p.opened_ts + horizon_ms + 1000), Some(TriggerAction::TimeStop));
        // SL takes precedence over time stop if both hit? Our impl checks SL first, so if mid at SL and time also, Sl returned.
        assert_eq!(check_triggers(&p, 98.0, p.opened_ts + horizon_ms), Some(TriggerAction::Sl));
    }

    #[test]
    fn default_horizon_24h() {
        let mut p = pos_long();
        p.horizon_hours = None;
        let horizon_ms = (24.0 * 3600.0 * 1000.0) as i64;
        assert_eq!(check_triggers(&p, 100.0, p.opened_ts + horizon_ms), Some(TriggerAction::TimeStop));
    }
}

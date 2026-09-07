#![allow(dead_code)]

use std::collections::HashSet;

use crate::{
    config::{ScreenerCfg, UniverseCfg},
    contracts::{MarketRow, Nominee, Side},
};

/// Pure screener implementing spec §5 verbatim.
/// Cross-sectional z-scores over vlm-filtered universe.
/// score = 2*|z(r1h)| + 1*|z(r5m)| + 1*|funding_z| + 0.5*range_edge
/// where range_edge = max(0, |range_pos-0.5|*2 -0.6)
/// side_hint = sign(r1h) flipped to fade when |funding_z|>2.5 and opposes.
pub fn screen(
    rows: &[MarketRow],
    cfg: &ScreenerCfg,
    universe: &UniverseCfg,
    excluded: &HashSet<String>,
) -> Vec<Nominee> {
    // 1. Filter by vlm and features and excluded
    let filtered: Vec<&MarketRow> = rows
        .iter()
        .filter(|r| r.features.is_some())
        .filter(|r| {
            let min_vlm = if r.market.starts_with("xyz:") {
                universe.min_vlm_dex
            } else {
                universe.min_vlm_native
            };
            r.day_ntl_vlm >= min_vlm
        })
        // Batch B allowlist (majors-only pilot): when non-empty, NATIVE markets must match
        // exactly; `xyz:` equities pass through on their volume floor alone.
        .filter(|r| {
            universe.allowlist.is_empty()
                || r.market.starts_with("xyz:")
                || universe.allowlist.iter().any(|m| m == &r.market)
        })
        .filter(|r| !excluded.contains(&r.market))
        .collect();

    if filtered.is_empty() {
        return Vec::new();
    }

    // 2. Compute z-scores over filtered set for r1h, r5m, funding_z
    let z_scores = |vals: &[f64]| -> Vec<f64> {
        if vals.len() <= 1 {
            return vec![0.0; vals.len()];
        }
        let mean = vals.iter().sum::<f64>() / vals.len() as f64;
        let var = vals.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / vals.len() as f64;
        let std = var.sqrt();
        if std < 1e-12 {
            vec![0.0; vals.len()]
        } else {
            vals.iter().map(|v| (v - mean) / std).collect()
        }
    };

    let r1h_vals: Vec<f64> = filtered.iter().map(|r| r.features.as_ref().expect("filtered").r1h).collect();
    let r5m_vals: Vec<f64> = filtered.iter().map(|r| r.features.as_ref().expect("filtered").r5m).collect();
    let funding_vals: Vec<f64> = filtered
        .iter()
        .map(|r| r.features.as_ref().expect("filtered").funding_z)
        .collect();

    let z_r1h = z_scores(&r1h_vals);
    let z_r5m = z_scores(&r5m_vals);
    let _z_funding = z_scores(&funding_vals);
    // Note: spec says funding_z is already a z, but formula uses |z(r?)|? Wait spec: score uses funding_z directly not cross-sectional? Actually spec: score=2*|z(r1h)|+1*|z(r5m)|+1*|funding_z|+0.5*range_edge where funding_z is per-market funding z (already). But plan Task7 says "cross-sectional z-scores over vlm-filtered universe; score=2*|z(r1h)|+..." So funding_z in score is |funding_z|? Or |z(funding_z)|? Spec §5: `1.0*|funding_z|` — funding_z is already z. So we should use absolute funding_z from features, not z-scored again. However Task 7 says "cross-sectional z-scores over the vlm-filtered set; score = 2·|z(r1h)| + 1·|z(r5m)| + 1·|funding_z| + 0.5·range_edge" That suggests first two are z-scored, funding_z is raw. Use raw funding_z.
    // We'll use raw funding_z for score contribution, but keep z_funding for potential alternative. Use raw.
    // To be safe, use raw funding_z absolute: |funding_z|

    let now_ts = chrono::Utc::now().timestamp_millis();

    let mut nominees: Vec<Nominee> = filtered
        .iter()
        .enumerate()
        .filter_map(|(i, row)| {
            let f = row.features.as_ref()?;
            let range_edge = ((f.range_pos - 0.5).abs() * 2.0 - 0.6).max(0.0);
            // Use cross-sectional z for r1h/r5m, raw funding_z for funding
            let score = 2.0 * z_r1h[i].abs() + z_r5m[i].abs() + f.funding_z.abs() + 0.5 * range_edge;
            if score < cfg.min_score {
                return None;
            }
            // side_hint = sign(r1h) flipped when funding_z extreme opposes
            let mut side = if f.r1h >= 0.0 { Side::Long } else { Side::Short };
            if f.funding_z.abs() > 2.5 {
                let funding_sign = if f.funding_z > 0.0 { 1 } else { -1 };
                let r1h_sign = if f.r1h >= 0.0 { 1 } else { -1 };
                if funding_sign != r1h_sign {
                    side = match side {
                        Side::Long => Side::Short,
                        Side::Short => Side::Long,
                    };
                }
            }
            Some(Nominee {
                ts: now_ts,
                market: row.market.clone(),
                side_hint: side,
                score,
                features: f.clone(),
            })
        })
        .collect();

    // Sort desc by score
    nominees.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    nominees.truncate(cfg.top_k);
    nominees
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ScreenerCfg, UniverseCfg};
    use crate::contracts::{Features, MarketRow};
    use std::collections::HashSet;

    fn mk_row(market: &str, vlm: f64, r1h: f64, r5m: f64, funding_z: f64, range_pos: f64) -> MarketRow {
        MarketRow {
            market: market.to_string(),
            mid: 100.0,
            mark: 100.0,
            oracle: 100.0,
            funding: 0.0,
            open_interest: 1000.0,
            day_ntl_vlm: vlm,
            prev_day_px: 99.0,
            features: Some(Features {
                r5m,
                r1h,
                r24h: r1h,
                vol1h: 0.4,
                funding_z,
                range_pos,
            }),
        }
    }

    fn cfg() -> ScreenerCfg {
        ScreenerCfg {
            interval_s: 45,
            top_k: 6,
            min_score: 1.8,
            skip_recheck_min: 15,
            skip_recheck_score_jump: 0.5,
        }
    }

    fn universe() -> UniverseCfg {
        UniverseCfg {
            dexs: vec!["".to_string(), "xyz".to_string()],
            min_vlm_native: 500_000.0,
            min_vlm_dex: 200_000.0,
            allowlist: vec![],
        }
    }

    #[test]
    fn momentum_outlier_ranks_first() {
        let mut rows = Vec::new();
        for i in 0..9 {
            rows.push(mk_row(&format!("M{i}"), 1_000_000.0, 0.1 * i as f64, 0.05 * i as f64, 0.0, 0.5));
        }
        // outlier with large r1h
        rows.push(mk_row("OUTLIER", 1_000_000.0, 5.0, 2.0, 0.0, 0.5));
        let nominees = screen(&rows, &cfg(), &universe(), &HashSet::new());
        assert!(!nominees.is_empty());
        assert_eq!(nominees[0].market, "OUTLIER");
        assert_eq!(nominees[0].side_hint, Side::Long);
    }

    #[test]
    fn funding_extreme_flips_side() {
        // Create universe where one market has high r1h long but funding extreme negative => should flip to short
        let mut rows = Vec::new();
        for i in 0..9 {
            rows.push(mk_row(&format!("M{i}"), 1_000_000.0, 0.0, 0.0, 0.0, 0.5));
        }
        // outlier long momentum but funding_z = -3.0 (extreme negative) opposes long => flip to short
        rows.push(mk_row("FLIP", 1_000_000.0, 2.0, 1.0, -3.0, 0.5));
        let nominees = screen(&rows, &cfg(), &universe(), &HashSet::new());
        let flip = nominees.iter().find(|n| n.market == "FLIP").expect("FLIP nominated");
        assert_eq!(flip.side_hint, Side::Short, "should flip due to funding extreme opposing");
        // Non-extreme funding should not flip
        let mut rows2 = Vec::new();
        for i in 0..9 {
            rows2.push(mk_row(&format!("M{i}"), 1_000_000.0, 0.0, 0.0, 0.0, 0.5));
        }
        rows2.push(mk_row("NOFLIP", 1_000_000.0, 2.0, 1.0, 1.0, 0.5));
        let nominees2 = screen(&rows2, &cfg(), &universe(), &HashSet::new());
        let noflip = nominees2.iter().find(|n| n.market == "NOFLIP").expect("NOFLIP");
        assert_eq!(noflip.side_hint, Side::Long);
    }

    #[test]
    fn below_vlm_never_nominated() {
        let rows = vec![
            mk_row("GOOD", 1_000_000.0, 5.0, 2.0, 0.0, 0.5),
            mk_row("LOW", 1000.0, 10.0, 5.0, 0.0, 0.5), // below 500k
            mk_row("XYZ_GOOD", 300_000.0, 4.0, 1.5, 0.0, 0.5), // xyz threshold 200k => passes if xyz:
            mk_row("xyz:LOW", 100_000.0, 10.0, 5.0, 0.0, 0.5),
        ];
        // adjust xyz names to have prefix
        let mut rows2 = rows;
        rows2[2].market = "xyz:TSLA".to_string();
        let nominees = screen(&rows2, &cfg(), &universe(), &HashSet::new());
        assert!(nominees.iter().all(|n| n.market != "LOW"));
        assert!(nominees.iter().all(|n| n.market != "xyz:LOW"));
        // GOOD and xyz:TSLA should be present
        assert!(nominees.iter().any(|n| n.market == "GOOD"));
        assert!(nominees.iter().any(|n| n.market == "xyz:TSLA"));
    }

    #[test]
    fn excluded_respected() {
        let rows = vec![
            mk_row("A", 1_000_000.0, 5.0, 2.0, 0.0, 0.9),
            mk_row("B", 1_000_000.0, 4.0, 1.5, 0.0, 0.9),
            mk_row("C", 1_000_000.0, 3.0, 1.0, 0.0, 0.9),
        ];
        let mut excluded = HashSet::new();
        excluded.insert("A".to_string());
        let nominees = screen(&rows, &cfg(), &universe(), &excluded);
        assert!(nominees.iter().all(|n| n.market != "A"));
        assert!(!nominees.is_empty());
    }

    #[test]
    fn allowlist_filters_native_memes_keeps_majors_and_xyz() {
        let uni = UniverseCfg {
            allowlist: vec!["BTC".to_string(), "ETH".to_string()],
            ..universe()
        };
        let rows = vec![
            mk_row("PURR", 10_000_000.0, 5.0, 2.0, 0.0, 0.5), // native meme, huge vlm
            mk_row("BTC", 10_000_000.0, 4.0, 1.5, 0.0, 0.5),  // allowlisted native
            mk_row("xyz:TSLA", 300_000.0, 4.5, 1.8, 0.0, 0.5), // dex: allowlist-exempt, above dex floor
            mk_row("xyz:THIN", 100_000.0, 6.0, 3.0, 0.0, 0.5), // dex below its vlm floor
        ];
        let nominees = screen(&rows, &cfg(), &uni, &HashSet::new());
        assert!(nominees.iter().all(|n| n.market != "PURR"), "native meme not on the allowlist is filtered");
        assert!(nominees.iter().any(|n| n.market == "BTC"), "allowlisted native kept");
        assert!(nominees.iter().any(|n| n.market == "xyz:TSLA"), "xyz: passes on its volume floor alone");
        assert!(nominees.iter().all(|n| n.market != "xyz:THIN"), "xyz: still bound by the dex volume floor");
        // empty allowlist = current behavior: PURR is nominable again
        let nominees_open = screen(&rows, &cfg(), &universe(), &HashSet::new());
        assert!(nominees_open.iter().any(|n| n.market == "PURR"), "empty allowlist disables the filter");
    }

    #[test]
    fn range_edge_contributes() {
        // Two markets with same momentum but different range_pos
        let rows = vec![
            mk_row("EDGE", 1_000_000.0, 1.0, 0.5, 0.0, 0.95), // near edge => range_edge = (0.45*2-0.6)=0.3
            mk_row("MID", 1_000_000.0, 1.0, 0.5, 0.0, 0.5),   // mid => 0
        ];
        // Add 8 more neutral to make z-scores meaningful
        let mut all = rows;
        for i in 0..8 {
            all.push(mk_row(&format!("N{i}"), 1_000_000.0, 0.0, 0.0, 0.0, 0.5));
        }
        let nominees = screen(&all, &cfg(), &universe(), &HashSet::new());
        // EDGE should score higher than MID due to range_edge
        let edge_score = nominees.iter().find(|n| n.market == "EDGE").map(|n| n.score).unwrap_or(0.0);
        let mid_score = nominees.iter().find(|n| n.market == "MID").map(|n| n.score).unwrap_or(0.0);
        assert!(edge_score > mid_score, "edge {edge_score} should > mid {mid_score}");
    }
}

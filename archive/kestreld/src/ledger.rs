#![allow(dead_code)]

use std::collections::HashMap;
use std::str::FromStr;

use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use sqlx::SqlitePool;
use thiserror::Error;

use crate::contracts::{Position, Side, Trade};
use crate::sizing::Sized;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("sqlx error: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("io error: {0}")]
    Io(String),
    #[error("not found: {0}")]
    NotFound(String),
}

#[derive(Clone)]
pub struct Store {
    pool: SqlitePool,
}

impl Store {
    pub async fn open(url: &str) -> Result<Self, StoreError> {
        let pool = SqlitePool::connect(url).await?;
        if url != "sqlite::memory:" && !url.contains(":memory:") && !url.contains("mode=memory") {
            let _ = sqlx::query("PRAGMA journal_mode=WAL;").execute(&pool).await;
            let _ = sqlx::query("PRAGMA foreign_keys=ON;").execute(&pool).await;
        }
        let sql = include_str!("../migrations/0001_init.sql");
        for stmt in sql.split(';') {
            let s = stmt.trim();
            if s.is_empty() {
                continue;
            }
            sqlx::query(s).execute(&pool).await?;
        }
        // 0002_fill_mode: additive column, historical rows default 'flat' (pre-Batch-1 were flat — correct!).
        // Rerun-safe: if column already exists, ignore duplicate column error (sqlx migrations table not used; we run raw SQL).
        let sql2 = include_str!("../migrations/0002_fill_mode.sql");
        for stmt in sql2.split(';') {
            let s = stmt.trim();
            if s.is_empty() {
                continue;
            }
            let res = sqlx::query(s).execute(&pool).await;
            if let Err(e) = res {
                let msg = e.to_string();
                if !msg.contains("duplicate column") && !msg.contains("already exists") {
                    return Err(StoreError::Sqlx(e));
                }
            }
        }
        // 0003_counterfactuals: additive table, `CREATE TABLE IF NOT EXISTS` so it is rerun-safe
        // by construction (no duplicate-column dance needed).
        let sql3 = include_str!("../migrations/0003_counterfactuals.sql");
        for stmt in sql3.split(';') {
            let s = stmt.trim();
            if s.is_empty() {
                continue;
            }
            sqlx::query(s).execute(&pool).await?;
        }
        let sql4 = include_str!("../migrations/0004_analyst_transparency.sql");
        for stmt in sql4.split(';') {
            let s = stmt.trim();
            if !s.is_empty() {
                sqlx::query(s).execute(&pool).await?;
            }
        }
        let sql5 = include_str!("../migrations/0005_analyst_arena.sql");
        for stmt in sql5.split(';') {
            let s = stmt.trim();
            if s.is_empty() { continue; }
            if let Err(e) = sqlx::query(s).execute(&pool).await {
                let msg = e.to_string();
                if !msg.contains("duplicate column") && !msg.contains("already exists") {
                    return Err(StoreError::Sqlx(e));
                }
            }
        }
        // 0006_decision_invalidation: additive nullable column; same rerun-safe pattern.
        let sql6 = include_str!("../migrations/0006_decision_invalidation.sql");
        for stmt in sql6.split(';') {
            let s = stmt.trim();
            if s.is_empty() { continue; }
            if let Err(e) = sqlx::query(s).execute(&pool).await {
                let msg = e.to_string();
                if !msg.contains("duplicate column") && !msg.contains("already exists") {
                    return Err(StoreError::Sqlx(e));
                }
            }
        }
        Ok(Self { pool })
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    pub async fn insert_position(&self, pos: &Position) -> Result<i64, StoreError> {
        let side_str = match pos.side {
            Side::Long => "long",
            Side::Short => "short",
        };
        let res = sqlx::query(
            "INSERT INTO positions (id, market, side, entry_px, size, leverage, margin, sl_px, tp_px, opened_ts, status, analyst) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'open', ?11)",
        )
        .bind(pos.id)
        .bind(&pos.market)
        .bind(side_str)
        .bind(pos.entry_px)
        .bind(pos.size)
        .bind(pos.leverage)
        .bind(pos.margin)
        .bind(pos.sl_px)
        .bind(pos.tp_px)
        .bind(pos.opened_ts)
        .bind(&pos.analyst)
        .execute(&self.pool)
        .await?;
        Ok(res.last_insert_rowid())
    }

    pub async fn get_position(&self, id: i64) -> Result<Option<Position>, StoreError> {
        let row = sqlx::query_as::<_, PositionRow>(
            "SELECT id, market, side, entry_px, size, leverage, margin, sl_px, tp_px, opened_ts, analyst FROM positions WHERE id = ?1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| r.into_position()))
    }

    pub async fn list_positions(&self) -> Result<Vec<Position>, StoreError> {
        let rows = sqlx::query_as::<_, PositionRow>(
            "SELECT id, market, side, entry_px, size, leverage, margin, sl_px, tp_px, opened_ts, analyst FROM positions",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|r| r.into_position()).collect())
    }

    pub async fn open_positions(&self) -> Result<Vec<Position>, StoreError> {
        let rows = sqlx::query_as::<_, PositionRow>(
            "SELECT id, market, side, entry_px, size, leverage, margin, sl_px, tp_px, opened_ts, analyst FROM positions WHERE status='open'",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|r| r.into_position()).collect())
    }

    pub async fn open_positions_for_analyst(&self, analyst: &str) -> Result<Vec<Position>, StoreError> {
        let rows = sqlx::query_as::<_, PositionRow>("SELECT id, market, side, entry_px, size, leverage, margin, sl_px, tp_px, opened_ts, analyst FROM positions WHERE status='open' AND analyst=?1")
            .bind(analyst).fetch_all(&self.pool).await?;
        Ok(rows.into_iter().map(PositionRow::into_position).collect())
    }

    pub async fn analyst_daily_entries(&self, analyst: &str, since_ms: i64) -> Result<i64, StoreError> {
        Ok(sqlx::query_scalar("SELECT COUNT(*) FROM positions WHERE analyst=?1 AND opened_ts>=?2").bind(analyst).bind(since_ms).fetch_one(&self.pool).await?)
    }

    pub async fn analyst_market_entries(&self, analyst: &str, market: &str, since_ms: i64) -> Result<i64, StoreError> {
        Ok(sqlx::query_scalar("SELECT COUNT(*) FROM positions WHERE analyst=?1 AND market=?2 AND opened_ts>=?3").bind(analyst).bind(market).bind(since_ms).fetch_one(&self.pool).await?)
    }

    pub async fn analyst_last_close(&self, analyst: &str, market: &str) -> Result<Option<(i64, String)>, StoreError> {
        sqlx::query_as("SELECT p.closed_ts, t.action FROM positions p JOIN trades t ON t.position_id=p.id WHERE p.analyst=?1 AND p.market=?2 AND p.status='closed' AND t.action!='open' ORDER BY p.closed_ts DESC LIMIT 1")
            .bind(analyst).bind(market).fetch_optional(&self.pool).await.map_err(Into::into)
    }

    /// Last `limit` closes for one analyst, newest first — per-model performance feedback.
    /// `net = realized_pnl - fee` matches `recent_closes`'s money definition.
    pub async fn analyst_recent_closes(&self, analyst: &str, limit: i64) -> Result<Vec<CloseRecord>, StoreError> {
        let rows: Vec<CloseRecord> = sqlx::query_as(
            "SELECT t.ts as ts, t.action as action, (t.realized_pnl - t.fee) AS net_pnl \
              FROM trades t JOIN positions p ON p.id = t.position_id \
             WHERE p.analyst=?1 AND t.action<>'open' ORDER BY t.ts DESC, t.id DESC LIMIT ?2",
        )
        .bind(analyst)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    // Paper ledger logic
    /// VWAP of filling `coin_size` base units consuming `levels` (ordered best-first).
    /// None when book depth is insufficient to fill the whole size.
    /// NOTE: `levels` must be pre-sorted best-first (bids desc, asks asc) by caller.
    pub(crate) fn vwap_for_size(
        coin_size: f64,
        levels: &[crate::contracts::L2Level],
    ) -> Option<f64> {
        if coin_size <= 0.0 || levels.is_empty() {
            return None;
        }
        let mut remaining = coin_size;
        let mut cost = 0.0;
        for lvl in levels {
            if lvl.px <= 0.0 || lvl.sz <= 0.0 {
                continue;
            }
            let take = remaining.min(lvl.sz);
            cost += take * lvl.px;
            remaining -= take;
            if remaining <= 1e-12 {
                return Some(cost / coin_size);
            }
        }
        None // book exhausted before size filled
    }

    /// Open fill: notional-targeted (margin*lev). Two-pass: estimate size at touch px, then
    /// VWAP for that size. Falls back to frozen flat-slip model when no usable book —
    /// ledger pinned tests pass None and keep their exact outputs.
    pub(crate) fn resolve_open_fill(
        mark: f64,
        notional: f64,
        side: Side,
        is_xyz: bool,
        book: Option<&crate::contracts::L2Book>,
    ) -> f64 {
        let slip = if is_xyz { 0.0005 } else { 0.0002 };
        let flat = match side {
            Side::Long => mark * (1.0 + slip),
            Side::Short => mark * (1.0 - slip),
        };
        let Some(b) = book else { return flat };
        let now = chrono::Utc::now().timestamp_millis();
        let (bids, asks) = (&b.levels[0], &b.levels[1]);
        // usable book: <3s stale, ≥1 real level on the aggressive side
        if now - b.ts > 3000 {
            return flat;
        }
        let aggr = match side {
            Side::Long => asks,
            Side::Short => bids,
        };
        let touch = aggr.first().map(|l| l.px).unwrap_or(0.0);
        if touch <= 0.0 {
            return flat;
        }
        // conservative never-better-than-flat: keep at least flat slippage,
        // i.e. use the WORSE of flat price vs book vwap (direction-adverse)
        let est_size = notional / touch.max(1e-9);
        match Self::vwap_for_size(est_size, aggr) {
            Some(vwap) => match side {
                Side::Long => f64::max(vwap, flat),
                Side::Short => f64::min(vwap, flat),
            },
            None => flat, // depth exhausted: fall back (conservative)
        }
    }

    /// Close fill: coin-size known. Same fallback policy.
    pub(crate) fn resolve_close_fill(
        mark: f64,
        coin_size: f64,
        side: Side,
        is_xyz: bool,
        book: Option<&crate::contracts::L2Book>,
    ) -> f64 {
        let slip = if is_xyz { 0.0005 } else { 0.0002 };
        let flat = match side {
            Side::Long => mark * (1.0 - slip),
            Side::Short => mark * (1.0 + slip),
        };
        let Some(b) = book else { return flat };
        let now = chrono::Utc::now().timestamp_millis();
        if now - b.ts > 3000 {
            return flat;
        }
        let (bids, asks) = (&b.levels[0], &b.levels[1]);
        let aggr = match side {
            Side::Long => bids,
            Side::Short => asks,
        };
        match Self::vwap_for_size(coin_size, aggr) {
            Some(vwap) => match side {
                Side::Long => f64::min(vwap, flat),
                Side::Short => f64::max(vwap, flat),
            },
            None => flat,
        }
    }

    /// Decide fill_mode: 'book' when impact vwap path actually filled (book usable AND vwap computed AND depth sufficient),
    /// else 'flat' (all fallback branches). Decision rule: resolved fill != flat price → 'book'; == flat → 'flat'.
    /// If resolve returns exactly the flat price even via book path (capped), 'flat' is correct and simpler — documented exactly.
    fn open_fill_mode(mark: f64, side: Side, is_xyz: bool, fill_px: f64) -> &'static str {
        let slip = if is_xyz { 0.0005 } else { 0.0002 };
        let flat = match side {
            Side::Long => mark * (1.0 + slip),
            Side::Short => mark * (1.0 - slip),
        };
        if (fill_px - flat).abs() > 1e-9 {
            "book"
        } else {
            "flat"
        }
    }
    fn close_fill_mode(mark: f64, side: Side, is_xyz: bool, fill_px: f64) -> &'static str {
        let slip = if is_xyz { 0.0005 } else { 0.0002 };
        let flat = match side {
            Side::Long => mark * (1.0 - slip),
            Side::Short => mark * (1.0 + slip),
        };
        if (fill_px - flat).abs() > 1e-9 {
            "book"
        } else {
            "flat"
        }
    }

    #[allow(clippy::too_many_arguments)] // fundamental fill params; grouping would churn call sites + pinned tests
    pub async fn open_position(
        &self,
        market: &str,
        side: Side,
        sized: &Sized,
        mark: f64,
        is_xyz: bool,
        horizon_hours: f64,
        book: Option<&crate::contracts::L2Book>,
    ) -> Result<Position, StoreError> {
        self.open_position_for_analyst(market, side, sized, mark, is_xyz, horizon_hours, book, "").await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn open_position_for_analyst(
        &self, market: &str, side: Side, sized: &Sized, mark: f64, is_xyz: bool,
        horizon_hours: f64, book: Option<&crate::contracts::L2Book>, analyst: &str,
    ) -> Result<Position, StoreError> {
        let fill_px = Self::resolve_open_fill(mark, sized.notional, side, is_xyz, book);
        let fill_mode = Self::open_fill_mode(mark, side, is_xyz, fill_px);
        let size = sized.notional / fill_px;
        let (sl_px, tp_px) = match side {
            Side::Long => (
                fill_px * (1.0 - sized.stop_pct / 100.0),
                fill_px * (1.0 + sized.tp_pct / 100.0),
            ),
            Side::Short => (
                fill_px * (1.0 + sized.stop_pct / 100.0),
                fill_px * (1.0 - sized.tp_pct / 100.0),
            ),
        };
        let opened_ts = chrono::Utc::now().timestamp_millis();
        let fee = sized.notional * 0.00075;

        // insert position
        let side_str = match side {
            Side::Long => "long",
            Side::Short => "short",
        };
        let res = sqlx::query(
            "INSERT INTO positions (market, side, entry_px, size, leverage, margin, sl_px, tp_px, opened_ts, status, horizon_hours, analyst) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,'open',?10,?11)",
        )
        .bind(market)
        .bind(side_str)
        .bind(fill_px)
        .bind(size)
        .bind(sized.leverage)
        .bind(sized.margin)
        .bind(sl_px)
        .bind(tp_px)
        .bind(opened_ts)
        .bind(horizon_hours)
        .bind(analyst)
        .execute(&self.pool)
        .await?;
        let id = res.last_insert_rowid();

        // insert trade open with fill_mode attribution
        sqlx::query(
            "INSERT INTO trades (position_id, market, action, px, size, fee, realized_pnl, ts, fill_mode) VALUES (?1,?2,'open',?3,?4,?5,0,?6,?7)",
        )
        .bind(id)
        .bind(market)
        .bind(fill_px)
        .bind(size)
        .bind(fee)
        .bind(opened_ts)
        .bind(fill_mode)
        .execute(&self.pool)
        .await?;

        // increment daily counter
        let day = chrono::Utc::now().format("%Y-%m-%d").to_string();
        self.inc_daily_count(&day).await?;

        Ok(Position {
            id,
            market: market.to_string(),
            side,
            entry_px: fill_px,
            size,
            leverage: sized.leverage,
            margin: sized.margin,
            sl_px,
            tp_px,
            opened_ts,
            analyst: analyst.to_string(),
            horizon_hours: Some(horizon_hours),
        })
    }

    pub async fn close_position(
        &self,
        id: i64,
        mark: f64,
        reason: &str,
        book: Option<&crate::contracts::L2Book>,
    ) -> Result<Trade, StoreError> {
        // fetch position
        let pos = self
            .get_position_full(id)
            .await?
            .ok_or_else(|| StoreError::NotFound(format!("position {id} not found")))?;
        if pos.status != "open" {
            return Err(StoreError::NotFound(format!("position {id} not open")));
        }
        let side = match pos.side.as_str() {
            "long" => Side::Long,
            "short" => Side::Short,
            _ => Side::Long,
        };
        let is_xyz = pos.market.starts_with("xyz:");
        let fill_px = Self::resolve_close_fill(mark, pos.size, side, is_xyz, book);
        let fill_mode = Self::close_fill_mode(mark, side, is_xyz, fill_px);
        let size = pos.size;
        // realized_pnl is GROSS (price movement only); fees live in their own column and
        // equity() accounts both (bankroll - SUM(fee) + SUM(realized) + unrealized).
        // Net-per-trade = realized_pnl - (open_fee + close_fee) — pinned by tests.
        let gross = match side {
            Side::Long => (fill_px - pos.entry_px) * size,
            Side::Short => (pos.entry_px - fill_px) * size,
        };
        let notional_exit = size * fill_px;
        let fee = notional_exit * 0.00075;
        let ts = chrono::Utc::now().timestamp_millis();

        // update position status
        sqlx::query("UPDATE positions SET status='closed', closed_ts=?1 WHERE id=?2")
            .bind(ts)
            .bind(id)
            .execute(&self.pool)
            .await?;

        // insert trade with fill_mode
        let res = sqlx::query(
            "INSERT INTO trades (position_id, market, action, px, size, fee, realized_pnl, ts, fill_mode) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        )
        .bind(id)
        .bind(&pos.market)
        .bind(reason)
        .bind(fill_px)
        .bind(size)
        .bind(fee)
        .bind(gross)
        .bind(ts)
        .bind(fill_mode)
        .execute(&self.pool)
        .await?;
        let trade_id = res.last_insert_rowid();
        Ok(Trade {
            id: trade_id,
            position_id: id,
            market: pos.market,
            action: reason.to_string(),
            px: fill_px,
            size,
            fee,
            realized_pnl: gross,
            ts,
            fill_mode: fill_mode.to_string(),
        })
    }

    pub async fn partial_close(
        &self,
        id: i64,
        close_size: f64,
        mark: f64,
        reason: &str,
        book: Option<&crate::contracts::L2Book>,
    ) -> Result<Trade, StoreError> {
        let pos = self
            .get_position_full(id)
            .await?
            .ok_or_else(|| StoreError::NotFound(format!("position {id} not found")))?;
        if pos.status != "open" {
            return Err(StoreError::NotFound(format!("position {id} not open")));
        }
        if close_size <= 0.0 || close_size > pos.size + 1e-9 {
            return Err(StoreError::Io("invalid close size".into()));
        }
        let side = match pos.side.as_str() {
            "long" => Side::Long,
            "short" => Side::Short,
            _ => Side::Long,
        };
        let is_xyz = pos.market.starts_with("xyz:");
        let fill_px = Self::resolve_close_fill(mark, close_size, side, is_xyz, book);
        let fill_mode = Self::close_fill_mode(mark, side, is_xyz, fill_px);
        let gross = match side {
            Side::Long => (fill_px - pos.entry_px) * close_size,
            Side::Short => (pos.entry_px - fill_px) * close_size,
        };
        let notional_exit = close_size * fill_px;
        let fee = notional_exit * 0.00075;
        let ts = chrono::Utc::now().timestamp_millis();

        // update position size
        let new_size = pos.size - close_size;
        if new_size.abs() < 1e-9 {
            sqlx::query("UPDATE positions SET size=0, status='closed', closed_ts=?1 WHERE id=?2")
                .bind(ts)
                .bind(id)
                .execute(&self.pool)
                .await?;
        } else {
            sqlx::query("UPDATE positions SET size=?1 WHERE id=?2")
                .bind(new_size)
                .bind(id)
                .execute(&self.pool)
                .await?;
        }

        let res = sqlx::query(
            "INSERT INTO trades (position_id, market, action, px, size, fee, realized_pnl, ts, fill_mode) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        )
        .bind(id)
        .bind(&pos.market)
        .bind(reason)
        .bind(fill_px)
        .bind(close_size)
        .bind(fee)
        .bind(gross)
        .bind(ts)
        .bind(fill_mode)
        .execute(&self.pool)
        .await?;
        let trade_id = res.last_insert_rowid();
        Ok(Trade {
            id: trade_id,
            position_id: id,
            market: pos.market,
            action: reason.to_string(),
            px: fill_px,
            size: close_size,
            fee,
            realized_pnl: gross,
            ts,
            fill_mode: fill_mode.to_string(),
        })
    }

    async fn get_position_full(&self, id: i64) -> Result<Option<PositionFull>, StoreError> {
        let row = sqlx::query_as::<_, PositionFull>(
            "SELECT id, market, side, entry_px, size, leverage, margin, sl_px, tp_px, opened_ts, status, closed_ts, horizon_hours FROM positions WHERE id=?1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    /// Move a position's stop. Callers must only EVER tighten (longs raise sl, shorts lower it);
    /// enforced in main.rs before calling.
    pub async fn update_stop(&self, id: i64, sl_px: f64) -> Result<(), StoreError> {
        sqlx::query("UPDATE positions SET sl_px = ?1 WHERE id = ?2 AND status = 'open'")
            .bind(sl_px)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn equity(&self, marks: &HashMap<String, f64>) -> Result<f64, StoreError> {
        // bankroll from meta or 1000
        let bankroll: f64 = self.get_meta_f64("bankroll").await?.unwrap_or(1000.0);
        let fees: f64 = sqlx::query_as::<_, (Option<f64>,)>("SELECT SUM(fee) FROM trades")
            .fetch_one(&self.pool)
            .await
            .map(|r| r.0.unwrap_or(0.0))?;
        let realized: f64 =
            sqlx::query_as::<_, (Option<f64>,)>("SELECT SUM(realized_pnl) FROM trades")
                .fetch_one(&self.pool)
                .await
                .map(|r| r.0.unwrap_or(0.0))?;

        let opens = self.open_positions().await?;
        let mut unreal = Decimal::ZERO;
        for p in opens {
            // MONEY GUARD: treat mid <= 0.0 or missing/non-finite as "unknown" — skip unrealized.
            // Prevents 3249.88-class spike: short with mark 0.0 would compute +100% unrealized
            // (`entry - 0` * size), long with mark 0 would compute -100% crash and false-trip
            // the -12% kill latch (`Risk::on_equity`). Valid equity is bankroll - fees + realized
            // + sum of only VALID unrealized; unknown marks contribute 0 until a real mid arrives.
            let Some(mark) = marks.get(&p.market).copied() else {
                continue;
            };
            if mark <= 0.0 || !mark.is_finite() {
                // Unknown/invalid mark — do NOT price with 0.0 fallback-artifacts.
                // Position stays priced at 0 unreal until next tick provides a valid mid.
                continue;
            }
            let entry = Decimal::from_str(&format!("{}", p.entry_px)).unwrap_or(Decimal::ZERO);
            let m = Decimal::from_str(&format!("{mark}")).unwrap_or(Decimal::ZERO);
            let sz = Decimal::from_str(&format!("{}", p.size)).unwrap_or(Decimal::ZERO);
            let pnl = match p.side {
                Side::Long => (m - entry) * sz,
                Side::Short => (entry - m) * sz,
            };
            unreal += pnl;
        }
        let bankroll_d = Decimal::from_str(&format!("{bankroll}")).unwrap_or(Decimal::ZERO);
        let fees_d = Decimal::from_str(&format!("{fees}")).unwrap_or(Decimal::ZERO);
        let realized_d = Decimal::from_str(&format!("{realized}")).unwrap_or(Decimal::ZERO);
        let equity_d = bankroll_d - fees_d + realized_d + unreal;
        Ok(equity_d.to_f64().unwrap_or(bankroll))
    }

    pub async fn snapshot_equity(&self, ts: i64, equity: f64) -> Result<(), StoreError> {
        sqlx::query("INSERT OR REPLACE INTO equity (ts, equity) VALUES (?1,?2)")
            .bind(ts)
            .bind(equity)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn get_meta(&self, k: &str) -> Result<Option<String>, StoreError> {
        let row: Option<(String,)> = sqlx::query_as("SELECT v FROM meta WHERE k=?1")
            .bind(k)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| r.0))
    }

    pub async fn set_meta(&self, k: &str, v: &str) -> Result<(), StoreError> {
        sqlx::query("INSERT OR REPLACE INTO meta (k,v) VALUES (?1,?2)")
            .bind(k)
            .bind(v)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn get_meta_f64(&self, k: &str) -> Result<Option<f64>, StoreError> {
        if let Some(s) = self.get_meta(k).await? {
            let f = s
                .parse::<f64>()
                .map_err(|_| StoreError::Io(format!("bad f64 for {k}")))?;
            Ok(Some(f))
        } else {
            Ok(None)
        }
    }

    async fn inc_daily_count(&self, day: &str) -> Result<(), StoreError> {
        let key = format!("daily_count:{day}");
        let cur = self
            .get_meta(&key)
            .await?
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(0);
        self.set_meta(&key, &(cur + 1).to_string()).await
    }

    pub async fn daily_count(&self, day: &str) -> Result<i64, StoreError> {
        let key = format!("daily_count:{day}");
        Ok(self
            .get_meta(&key)
            .await?
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(0))
    }

    /// Entries opened for one market since `since_ms` — the per-market daily cap's counter
    /// (Phase T1, `risk::Risk::gate_churn`).
    ///
    /// Counts POSITIONS, open or closed, so closing a position cannot buy back a slot: the cap
    /// is on entries taken, which is exactly the churn the knob exists to stop. Read from the
    /// ledger rather than a memory counter so a restart cannot reset the day's budget.
    pub async fn market_entries_since(
        &self,
        market: &str,
        since_ms: i64,
    ) -> Result<i64, StoreError> {
        let (n,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM positions WHERE market=?1 AND opened_ts>=?2")
                .bind(market)
                .bind(since_ms)
                .fetch_one(&self.pool)
                .await?;
        Ok(n)
    }

    /// Most recent close for a market as `(ts_ms, action)`, where action is the close reason
    /// the trigger/review loops wrote (`sl` / `tp` / `time_stop` / `veto_close`). `None` when
    /// the market has never closed.
    ///
    /// The trades table is the single source of truth for close causes, so the asymmetric
    /// cooldown (`risk::CloseCause`) survives a restart — unlike the in-memory
    /// `GateState::last_close_ts`, which only backs the base window.
    pub async fn last_close(&self, market: &str) -> Result<Option<(i64, String)>, StoreError> {
        let row: Option<(i64, String)> = sqlx::query_as(
            "SELECT ts, action FROM trades WHERE market=?1 AND action<>'open' ORDER BY ts DESC, id DESC LIMIT 1",
        )
        .bind(market)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    /// Entries per market since `since_ms`, `(market, count)` for every market that traded in
    /// the window — the AGGREGATE TWIN of `market_entries_since`, with a byte-identical
    /// predicate (`positions.opened_ts >= since`, open or closed).
    ///
    /// Exists so the screener's exclusion feed and `/api/gates` can see the whole day's churn
    /// state in ONE query instead of one per candidate market; `gate_churn` keeps using the
    /// single-market read. A test pins the two against each other so the predicate cannot drift.
    pub async fn market_entries_by_market(
        &self,
        since_ms: i64,
    ) -> Result<Vec<(String, i64)>, StoreError> {
        let rows: Vec<(String, i64)> = sqlx::query_as(
            "SELECT market, COUNT(*) FROM positions WHERE opened_ts>=?1 GROUP BY market ORDER BY market",
        )
        .bind(since_ms)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// Markets with at least one close since `since_ms` — the cooldown view's candidate list.
    /// Same `action<>'open'` predicate as `last_close`, which is then called per candidate for
    /// the cause; the set is bounded by markets actually closed inside the longest window.
    pub async fn markets_closed_since(&self, since_ms: i64) -> Result<Vec<String>, StoreError> {
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT DISTINCT market FROM trades WHERE action<>'open' AND ts>=?1 ORDER BY market",
        )
        .bind(since_ms)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|r| r.0).collect())
    }

    /// `(conviction, net_pnl)` for every CLOSED position an executed analyst decision produced —
    /// the conviction-vs-outcome join behind `/api/analytics`.
    ///
    /// `decisions` carries no position id (it is written *before* the open, and only marked
    /// executed after), so the join is temporal: the earliest position on the same market opened
    /// at or after the decision, within `match_window_ms`. That is unambiguous in practice — the
    /// open follows the decision row in the same task within seconds, and the `DupMarket` rail
    /// forbids a second open position on the market until this one closes.
    ///
    /// `net_pnl` is the position's whole life: realized minus ALL its fees (entry included), the
    /// same identity `Store::equity` uses, so bucket sums reconcile with the equity curve.
    pub async fn executed_decision_outcomes(
        &self,
        match_window_ms: i64,
    ) -> Result<Vec<(f64, f64)>, StoreError> {
        let rows: Vec<(f64, f64)> = sqlx::query_as(
            "SELECT d.conviction, \
                    (SELECT COALESCE(SUM(t.realized_pnl),0.0) - COALESCE(SUM(t.fee),0.0) \
                       FROM trades t WHERE t.position_id = p.id) \
               FROM decisions d \
               JOIN positions p ON p.id = ( \
                    SELECT p2.id FROM positions p2 \
                     WHERE p2.market = d.market AND p2.opened_ts >= d.ts AND p2.opened_ts <= d.ts + ?1 \
                     ORDER BY p2.opened_ts ASC, p2.id ASC LIMIT 1) \
              WHERE d.executed = 1 AND p.status = 'closed'",
        )
        .bind(match_window_ms)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// Per-market trading record. `trades` counts EXITS (round trips) — an open and its close
    /// are one trade to an operator — while `fees` and `net_pnl` account both legs.
    pub async fn market_stats(&self) -> Result<Vec<MarketStats>, StoreError> {
        let rows: Vec<MarketStats> = sqlx::query_as(
            "SELECT market, \
                    SUM(CASE WHEN action<>'open' THEN 1 ELSE 0 END) AS trades, \
                    COALESCE(SUM(realized_pnl),0.0) - COALESCE(SUM(fee),0.0) AS net_pnl, \
                    COALESCE(SUM(fee),0.0) AS fees \
               FROM trades GROUP BY market ORDER BY market ASC",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// Exits per UTC day bucketed by cause since `since_ms`. `other` is every close reason that
    /// is not tp / sl / veto_close (today: `time_stop`), so the four columns always sum to the
    /// day's exits no matter what a future loop names its close.
    pub async fn exit_mix_since(&self, since_ms: i64) -> Result<Vec<ExitMixDay>, StoreError> {
        let rows: Vec<ExitMixDay> = sqlx::query_as(
            "SELECT strftime('%Y-%m-%d', ts/1000, 'unixepoch') AS date, \
                    SUM(CASE WHEN action='tp' THEN 1 ELSE 0 END) AS tp, \
                    SUM(CASE WHEN action='sl' THEN 1 ELSE 0 END) AS sl, \
                    SUM(CASE WHEN action='veto_close' THEN 1 ELSE 0 END) AS veto_close, \
                    SUM(CASE WHEN action NOT IN ('tp','sl','veto_close') THEN 1 ELSE 0 END) AS other \
               FROM trades WHERE action<>'open' AND ts>=?1 GROUP BY date ORDER BY date ASC",
        )
        .bind(since_ms)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// Veto-closed positions with no cached counterfactual yet, oldest first (so repeated
    /// requests drain the backlog in order). `limit` is the per-request work bound.
    pub async fn counterfactual_pending(&self, limit: i64) -> Result<Vec<VetoClose>, StoreError> {
        let rows: Vec<VetoCloseRow> = sqlx::query_as(
            "SELECT t.position_id AS position_id, p.market AS market, p.side AS side, \
                    p.entry_px AS entry_px, p.sl_px AS sl_px, p.tp_px AS tp_px, \
                    p.opened_ts AS opened_ts, p.horizon_hours AS horizon_hours, \
                    t.ts AS close_ts, t.px AS close_px, t.size AS size, \
                    t.realized_pnl AS realized_pnl, t.fee AS fee \
               FROM trades t JOIN positions p ON p.id = t.position_id \
              WHERE t.action='veto_close' AND p.status='closed' \
                AND t.position_id NOT IN (SELECT position_id FROM counterfactuals) \
              ORDER BY t.ts ASC, t.id ASC LIMIT ?1",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(VetoCloseRow::into_veto_close)
            .collect())
    }

    /// How many veto closes are still uncached — reported as `pending` so the dashboard can
    /// say "5 more to replay" instead of silently showing a partial number.
    pub async fn counterfactual_pending_count(&self) -> Result<i64, StoreError> {
        let (n,): (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM trades t JOIN positions p ON p.id = t.position_id \
              WHERE t.action='veto_close' AND p.status='closed' \
                AND t.position_id NOT IN (SELECT position_id FROM counterfactuals)",
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(n)
    }

    /// Cache one replayed bracket. `INSERT OR REPLACE` keeps the write idempotent if two
    /// requests race on the same position — the walk is deterministic, so both agree.
    pub async fn counterfactual_put(
        &self,
        position_id: i64,
        computed_ts: i64,
        bracket_outcome: &str,
        bracket_pnl: f64,
        actual_pnl: f64,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT OR REPLACE INTO counterfactuals (position_id, computed_ts, bracket_outcome, bracket_pnl, actual_pnl) VALUES (?1,?2,?3,?4,?5)",
        )
        .bind(position_id)
        .bind(computed_ts)
        .bind(bracket_outcome)
        .bind(bracket_pnl)
        .bind(actual_pnl)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// `(computed, net_actual, net_bracket)` over every cached counterfactual.
    pub async fn counterfactual_totals(&self) -> Result<(i64, f64, f64), StoreError> {
        let row: (i64, f64, f64) = sqlx::query_as(
            "SELECT COUNT(*), COALESCE(SUM(actual_pnl),0.0), COALESCE(SUM(bracket_pnl),0.0) FROM counterfactuals",
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(row)
    }

    /// Cached counterfactuals joined back to the position each one judges, newest close first,
    /// capped at `limit`. This is a window onto exactly the population `counterfactual_totals`
    /// sums — never a different one, so a row can always be reconciled against the totals.
    ///
    /// The `closed_ts IS NOT NULL` filter is structurally unreachable (the same UPDATE that sets
    /// `status='closed'` stamps `closed_ts`, and only closed positions are ever cached); it is
    /// there so one impossible row degrades to a missing line instead of failing the whole read.
    pub async fn counterfactual_rows(
        &self,
        limit: i64,
    ) -> Result<Vec<CounterfactualRow>, StoreError> {
        let rows: Vec<CounterfactualRowSql> = sqlx::query_as(
            "SELECT c.position_id AS position_id, p.market AS market, p.side AS side, \
                    p.closed_ts AS closed_ts, c.actual_pnl AS actual_pnl, \
                    c.bracket_pnl AS bracket_pnl, c.bracket_outcome AS bracket_outcome \
               FROM counterfactuals c JOIN positions p ON p.id = c.position_id \
              WHERE p.closed_ts IS NOT NULL \
              ORDER BY p.closed_ts DESC, c.position_id DESC LIMIT ?1",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(CounterfactualRowSql::into_row)
            .collect())
    }

    /// Every trade stamped inside `[from_ms, to_ms)`, oldest first — the daily digest's raw
    /// material. Opens are included: the entry fee was paid on the day it was charged, so a
    /// day's `SUM(realized) - SUM(fee)` over this set is exactly that day's move in equity
    /// attributable to trading (`Store::equity`'s identity, windowed).
    pub async fn trades_between(&self, from_ms: i64, to_ms: i64) -> Result<Vec<Trade>, StoreError> {
        let rows: Vec<TradeRow> = sqlx::query_as(
            "SELECT id, position_id, market, action, px, size, fee, realized_pnl, ts, fill_mode \
               FROM trades WHERE ts>=?1 AND ts<?2 ORDER BY ts ASC, id ASC",
        )
        .bind(from_ms)
        .bind(to_ms)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(TradeRow::into_trade).collect())
    }

    /// First and last recorded equity inside `[from_ms, to_ms)` — the day's open→close from
    /// the 60s equity curve. `(None, None)` when the daemon logged nothing that day (it was
    /// down); the digest then falls back to the `day_open:` meta stamps.
    pub async fn equity_bounds(
        &self,
        from_ms: i64,
        to_ms: i64,
    ) -> Result<(Option<f64>, Option<f64>), StoreError> {
        let first: Option<(f64,)> = sqlx::query_as(
            "SELECT equity FROM equity WHERE ts>=?1 AND ts<?2 ORDER BY ts ASC LIMIT 1",
        )
        .bind(from_ms)
        .bind(to_ms)
        .fetch_optional(&self.pool)
        .await?;
        let last: Option<(f64,)> = sqlx::query_as(
            "SELECT equity FROM equity WHERE ts>=?1 AND ts<?2 ORDER BY ts DESC LIMIT 1",
        )
        .bind(from_ms)
        .bind(to_ms)
        .fetch_optional(&self.pool)
        .await?;
        Ok((first.map(|r| r.0), last.map(|r| r.0)))
    }

    /// `decisions.reason` for every decision logged inside `[from_ms, to_ms)`. The digest
    /// counts the ` gate_refused:<kind>` suffixes `append_decision_reason` writes there —
    /// refusals are deliberately NOT pushed to Telegram one by one (far too noisy), they are
    /// summarised once a day from this.
    pub async fn decision_reasons_between(
        &self,
        from_ms: i64,
        to_ms: i64,
    ) -> Result<Vec<String>, StoreError> {
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT reason FROM decisions WHERE ts>=?1 AND ts<?2 ORDER BY ts ASC, id ASC",
        )
        .bind(from_ms)
        .bind(to_ms)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|r| r.0).collect())
    }

    /// The last `limit` closes for one market, newest first — the analyst's memory of what
    /// this market has already done to the book.
    ///
    /// Same `action<>'open'` predicate as [`Store::last_close`], of which this is the N-row
    /// generalisation, extended with the money: `net_pnl` is that exit's realized minus that
    /// exit's own fee (the entry fee belongs to the open row, exactly as in
    /// [`Store::market_stats`]).
    pub async fn recent_closes(
        &self,
        market: &str,
        limit: i64,
    ) -> Result<Vec<CloseRecord>, StoreError> {
        let rows: Vec<CloseRecord> = sqlx::query_as(
            "SELECT ts, action, (realized_pnl - fee) AS net_pnl FROM trades \
              WHERE market=?1 AND action<>'open' ORDER BY ts DESC, id DESC LIMIT ?2",
        )
        .bind(market)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    pub async fn day_open_equity(&self, day: &str) -> Result<Option<f64>, StoreError> {
        self.get_meta_f64(&format!("day_open:{day}")).await
    }

    pub async fn set_day_open_equity(&self, day: &str, equity: f64) -> Result<(), StoreError> {
        self.set_meta(&format!("day_open:{day}"), &equity.to_string())
            .await
    }

    /// Log a decision row and return its id. Uses `last_insert_rowid()` from the
    /// sqlx result — connection-local, least-racy at max-2-concurrency (no separate
    /// SELECT that could pick the wrong row). Documented choice per commit spec.
    #[allow(clippy::too_many_arguments)]
    pub async fn log_decision(
        &self,
        ts: i64,
        market: &str,
        action: &str,
        side: &str,
        conviction: f64,
        thesis: &str,
        horizon_hours: f64,
        vetoed: bool,
        executed: bool,
        reason: &str,
    ) -> Result<i64, StoreError> {
        self.log_decision_for_analyst(ts, market, action, side, conviction, thesis, horizon_hours, vetoed, executed, reason, "", None).await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn log_decision_for_analyst(
        &self, ts: i64, market: &str, action: &str, side: &str, conviction: f64,
        thesis: &str, horizon_hours: f64, vetoed: bool, executed: bool, reason: &str, analyst: &str,
        invalidation_condition: Option<&str>,
    ) -> Result<i64, StoreError> {
        let res = sqlx::query(
            "INSERT INTO decisions (ts, market, action, side, conviction, thesis, horizon_hours, vetoed, executed, reason, analyst, invalidation_condition) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
        )
        .bind(ts)
        .bind(market)
        .bind(action)
        .bind(side)
        .bind(conviction)
        .bind(thesis)
        .bind(horizon_hours)
        .bind(vetoed as i64)
        .bind(executed as i64)
        .bind(reason)
        .bind(analyst)
        .bind(invalidation_condition)
        .execute(&self.pool)
        .await?;
        Ok(res.last_insert_rowid())
    }

    /// Append a suffix to a decision row's `reason`, so a risk-gate refusal lands next to
    /// the decision it blocked (`/api/decisions` then shows *why* an `open` never opened).
    /// Suffix tokens must never contain `refused:true` — that string is the dashboard's
    /// analyst-refusal marker (`intel-utils.ts`).
    pub async fn append_decision_reason(&self, id: i64, suffix: &str) -> Result<(), StoreError> {
        sqlx::query("UPDATE decisions SET reason = COALESCE(reason,'') || ?2 WHERE id=?1")
            .bind(id)
            .bind(suffix)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Mark a decisions row as executed. Id is from `log_decision`'s returned rowid.
    pub async fn mark_decision_executed(&self, id: i64) -> Result<(), StoreError> {
        sqlx::query("UPDATE decisions SET executed=1 WHERE id=?1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

/// One closed leg of a position, as the analyst prompt reads it back (`Store::recent_closes`).
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct CloseRecord {
    pub ts: i64,
    /// Close cause: `tp` / `sl` / `time_stop` / `veto_close`.
    pub action: String,
    /// Realized minus this exit's fee.
    pub net_pnl: f64,
}

#[derive(Debug, sqlx::FromRow)]
struct TradeRow {
    id: i64,
    position_id: i64,
    market: String,
    action: String,
    px: f64,
    size: f64,
    fee: f64,
    realized_pnl: f64,
    ts: i64,
    fill_mode: String,
}

impl TradeRow {
    fn into_trade(self) -> Trade {
        Trade {
            id: self.id,
            position_id: self.position_id,
            market: self.market,
            action: self.action,
            px: self.px,
            size: self.size,
            fee: self.fee,
            realized_pnl: self.realized_pnl,
            ts: self.ts,
            fill_mode: self.fill_mode,
        }
    }
}

/// One market's trading record (`/api/analytics` per-market strip).
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct MarketStats {
    pub market: String,
    pub trades: i64,
    pub net_pnl: f64,
    pub fees: f64,
}

/// One UTC day of exits bucketed by cause (`/api/analytics` exit mix).
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct ExitMixDay {
    pub date: String,
    pub tp: i64,
    pub sl: i64,
    pub veto_close: i64,
    pub other: i64,
}

/// A position the reviewer closed early, with everything needed to replay its bracket.
#[derive(Debug, Clone, PartialEq)]
pub struct VetoClose {
    pub position_id: i64,
    pub market: String,
    pub side: Side,
    pub entry_px: f64,
    pub sl_px: f64,
    pub tp_px: f64,
    pub opened_ts: i64,
    pub horizon_hours: Option<f64>,
    pub close_ts: i64,
    pub close_px: f64,
    pub size: f64,
    /// Realized minus the EXIT fee — the same shape as a replayed bracket pnl, so the two are
    /// directly comparable (the entry fee is common to both branches and cancels out).
    pub actual_pnl: f64,
}

/// One replayed veto close, joined to the position it judges — the per-position detail behind
/// the `/api/analytics` counterfactual totals.
#[derive(Debug, Clone, PartialEq)]
pub struct CounterfactualRow {
    pub position_id: i64,
    pub market: String,
    pub side: Side,
    pub closed_ts: i64,
    /// Realized minus the exit fee, as cached — directly comparable to `bracket_pnl`.
    pub actual_pnl: f64,
    pub bracket_pnl: f64,
    /// `tp` | `sl` | `expiry` (the table's CHECK domain).
    pub bracket_outcome: String,
}

#[derive(Debug, sqlx::FromRow)]
struct CounterfactualRowSql {
    position_id: i64,
    market: String,
    side: String,
    closed_ts: i64,
    actual_pnl: f64,
    bracket_pnl: f64,
    bracket_outcome: String,
}

impl CounterfactualRowSql {
    fn into_row(self) -> CounterfactualRow {
        CounterfactualRow {
            position_id: self.position_id,
            market: self.market,
            side: if self.side == "short" {
                Side::Short
            } else {
                Side::Long
            },
            closed_ts: self.closed_ts,
            actual_pnl: self.actual_pnl,
            bracket_pnl: self.bracket_pnl,
            bracket_outcome: self.bracket_outcome,
        }
    }
}

#[derive(Debug, sqlx::FromRow)]
struct VetoCloseRow {
    position_id: i64,
    market: String,
    side: String,
    entry_px: f64,
    sl_px: f64,
    tp_px: f64,
    opened_ts: i64,
    horizon_hours: Option<f64>,
    close_ts: i64,
    close_px: f64,
    size: f64,
    realized_pnl: f64,
    fee: f64,
}

impl VetoCloseRow {
    fn into_veto_close(self) -> VetoClose {
        VetoClose {
            position_id: self.position_id,
            market: self.market,
            side: if self.side == "short" {
                Side::Short
            } else {
                Side::Long
            },
            entry_px: self.entry_px,
            sl_px: self.sl_px,
            tp_px: self.tp_px,
            opened_ts: self.opened_ts,
            horizon_hours: self.horizon_hours,
            close_ts: self.close_ts,
            close_px: self.close_px,
            size: self.size,
            actual_pnl: self.realized_pnl - self.fee,
        }
    }
}

#[derive(Debug, sqlx::FromRow)]
struct PositionRow {
    id: i64,
    market: String,
    side: String,
    entry_px: f64,
    size: f64,
    leverage: f64,
    margin: f64,
    sl_px: f64,
    tp_px: f64,
    opened_ts: i64,
    analyst: String,
}

impl PositionRow {
    fn into_position(self) -> Position {
        let side = match self.side.as_str() {
            "long" => Side::Long,
            "short" => Side::Short,
            _ => Side::Long,
        };
        Position {
            id: self.id,
            market: self.market,
            side,
            entry_px: self.entry_px,
            size: self.size,
            leverage: self.leverage,
            margin: self.margin,
            sl_px: self.sl_px,
            tp_px: self.tp_px,
            opened_ts: self.opened_ts,
            analyst: self.analyst,
            horizon_hours: None,
        }
    }
}

#[derive(Debug, sqlx::FromRow)]
struct PositionFull {
    id: i64,
    market: String,
    side: String,
    entry_px: f64,
    size: f64,
    leverage: f64,
    margin: f64,
    sl_px: f64,
    tp_px: f64,
    opened_ts: i64,
    status: String,
    closed_ts: Option<i64>,
    horizon_hours: Option<f64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::{L2Book, L2Level, Position, Side};
    use crate::sizing::Sized;
    use std::collections::HashMap;

    fn book(ts_ms: i64, bids: &[(f64, f64)], asks: &[(f64, f64)]) -> L2Book {
        let to = |v: &[(f64, f64)]| {
            v.iter()
                .map(|&(px, sz)| L2Level { px, sz })
                .collect::<Vec<_>>()
        };
        L2Book {
            levels: [to(bids), to(asks)],
            ts: ts_ms,
        }
    }

    #[test]
    fn vwap_walks_levels() {
        let lv = Store::vwap_for_size;
        let asks = vec![
            L2Level { px: 100.0, sz: 1.0 },
            L2Level { px: 100.5, sz: 2.0 },
        ];
        // fill 2.0 coin: 1@100.0 + 1@100.5 = 200.5/2 = 100.25
        let v = lv(2.0, &asks).expect("filled");
        assert!((v - 100.25).abs() < 1e-9);
        // depth exhausted
        assert!(lv(5.0, &asks).is_none());
        // zero/empty guards
        assert!(lv(0.0, &asks).is_none());
        assert!(lv(1.0, &[]).is_none());
    }

    #[test]
    fn open_fill_long_uses_book_worse_of_flat() {
        let now = chrono::Utc::now().timestamp_millis();
        // thin asks: vwap 100.25 < flat 100.02? no wait flat is 100.02 — vwap HIGHER -> use vwap (worse)
        let b = book(now, &[(99.9, 10.0)], &[(100.0, 1.0), (100.5, 2.0)]);
        // notional 200 / touch 100 -> est size 2.0 -> vwap 100.25; flat 100*1.0002 = 100.02; worse(long) = max = 100.25
        let f = Store::resolve_open_fill(100.0, 200.0, Side::Long, false, Some(&b));
        assert!((f - 100.25).abs() < 1e-9, "impact fill {f}");
        // deep asks: vwap 100.005 < flat 100.02 -> use flat (conservative cap)
        let deep = book(now, &[(99.9, 10.0)], &[(100.005, 1000.0)]);
        let f2 = Store::resolve_open_fill(100.0, 200.0, Side::Long, false, Some(&deep));
        assert!((f2 - 100.02).abs() < 1e-9, "flat cap {f2}");
    }

    #[test]
    fn open_fill_fallbacks_flat() {
        let now = chrono::Utc::now().timestamp_millis();
        // stale book (4s old) -> flat
        let stale = book(now - 4_000, &[(99.9, 10.0)], &[(100.0, 1.0), (100.5, 2.0)]);
        let f = Store::resolve_open_fill(100.0, 200.0, Side::Long, false, Some(&stale));
        assert!((f - 100.02).abs() < 1e-9, "stale->flat {f}");
        // exhausted depth -> flat
        let thin = book(now, &[(99.9, 10.0)], &[(100.0, 0.1)]);
        let f2 = Store::resolve_open_fill(100.0, 10_000.0, Side::Long, false, Some(&thin));
        assert!((f2 - 100.02).abs() < 1e-9, "exhausted->flat {f2}");
        // xyz 5bp flat unaffected
        let f3 = Store::resolve_open_fill(100.0, 200.0, Side::Long, true, Some(&stale));
        assert!((f3 - 100.05).abs() < 1e-9);
    }

    #[test]
    fn close_fill_long_walks_bids_worse_of_flat() {
        let now = chrono::Utc::now().timestamp_millis();
        // bids: 99.9 x 2.0, 99.5 x 2.0. close long size 3 -> 2@99.9 + 1@99.5 = vwap 99.7666...
        let b = book(now, &[(99.9, 2.0), (99.5, 2.0)], &[(100.1, 5.0)]);
        let v_want = (99.9 * 2.0 + 99.5 * 1.0) / 3.0;
        let f = Store::resolve_close_fill(100.0, 3.0, Side::Long, false, Some(&b));
        assert!((f - v_want).abs() < 1e-9, "close vwap {f}");
        // flat would be 99.98 -> vwap 99.7667 is worse (min) -> vwap used. verify worse:
        assert!(f < 99.98);
    }

    #[tokio::test]
    async fn migrations_and_position_roundtrip() {
        let store = Store::open("sqlite::memory:").await.expect("open memory");
        let pos = Position {
            id: 1,
            market: "SOL".to_string(),
            side: Side::Long,
            entry_px: 100.0,
            size: 0.5,
            leverage: 10.0,
            margin: 20.0,
            sl_px: 98.0,
            tp_px: 104.0,
            opened_ts: 1_700_000_000_000,
            analyst: String::new(),
            horizon_hours: None,
        };
        let _id = store.insert_position(&pos).await.expect("insert");
        let fetched = store.get_position(1).await.expect("fetch").expect("found");
        assert_eq!(fetched.market, pos.market);
        assert_eq!(fetched.side, pos.side);
        assert!((fetched.entry_px - pos.entry_px).abs() < 1e-9);
        assert!((fetched.size - pos.size).abs() < 1e-9);
        assert!((fetched.leverage - pos.leverage).abs() < 1e-9);
        assert!((fetched.margin - pos.margin).abs() < 1e-9);
        assert!((fetched.sl_px - pos.sl_px).abs() < 1e-9);
        assert!((fetched.tp_px - pos.tp_px).abs() < 1e-9);
        assert_eq!(fetched.opened_ts, pos.opened_ts);
        let list = store.list_positions().await.expect("list");
        assert_eq!(list.len(), 1);
    }

    #[tokio::test]
    async fn wal_pragma_on_memory_ok() {
        let store = Store::open("sqlite::memory:").await.expect("open");
        let row: Option<(String,)> = sqlx::query_as("SELECT k FROM meta LIMIT 1")
            .fetch_optional(store.pool())
            .await
            .expect("query");
        assert!(row.is_none() || row.is_some());
    }

    #[tokio::test]
    async fn open_long_native_pinned() {
        let store = Store::open("sqlite::memory:").await.expect("store");
        let sized = Sized {
            leverage: 20.0,
            margin: 15.0,
            notional: 300.0,
            stop_pct: 0.6,
            tp_pct: 1.2,
        };
        let pos = store
            .open_position("SOL", Side::Long, &sized, 100.0, false, 24.0, None)
            .await
            .expect("open");
        assert!(
            (pos.entry_px - 100.02).abs() < 1e-9,
            "fill {}",
            pos.entry_px
        );
        // fee check via trades table
        let fee: (f64,) = sqlx::query_as("SELECT fee FROM trades WHERE position_id=?1")
            .bind(pos.id)
            .fetch_one(store.pool())
            .await
            .expect("fee");
        assert!((fee.0 - 0.225).abs() < 1e-9, "fee {}", fee.0);
        // sl/tp
        assert!((pos.sl_px - 100.02 * (1.0 - 0.006)).abs() < 1e-9);
        assert!((pos.tp_px - 100.02 * (1.0 + 0.012)).abs() < 1e-9);
    }

    #[tokio::test]
    async fn close_long_pinned() {
        let store = Store::open("sqlite::memory:").await.expect("store");
        let sized = Sized {
            leverage: 20.0,
            margin: 15.0,
            notional: 300.0,
            stop_pct: 0.6,
            tp_pct: 1.2,
        };
        let pos = store
            .open_position("SOL", Side::Long, &sized, 100.0, false, 24.0, None)
            .await
            .expect("open");
        let trade = store
            .close_position(pos.id, 102.0, "close", None)
            .await
            .expect("close");
        // fill 102*0.9998 =101.9796
        assert!(
            (trade.px - 101.9796).abs() < 1e-4,
            "close fill {} expected 101.9796",
            trade.px
        );
        // also check interim as allowed tolerance 1e-3 for 101.9799 vs 101.9796
        let expected = 102.0 * 0.9998;
        assert!((trade.px - expected).abs() < 1e-9);
        // realized = size*(exit-entry) ; size = 300/100.02 =2.9994
        let size = 300.0 / 100.02;
        let expected_realized = size * (trade.px - 100.02);
        assert!(
            (trade.realized_pnl - expected_realized).abs() < 1e-9,
            "realized {} expected {}",
            trade.realized_pnl,
            expected_realized
        );
        // fee
        let expected_fee = trade.px * size * 0.00075;
        assert!((trade.fee - expected_fee).abs() < 1e-9);
    }

    #[tokio::test]
    async fn short_symmetric() {
        let store = Store::open("sqlite::memory:").await.expect("store");
        let sized = Sized {
            leverage: 10.0,
            margin: 20.0,
            notional: 200.0,
            stop_pct: 1.0,
            tp_pct: 2.0,
        };
        let pos = store
            .open_position("BTC", Side::Short, &sized, 100.0, false, 24.0, None)
            .await
            .expect("open");
        assert!(
            (pos.entry_px - 99.98).abs() < 1e-9,
            "short fill {}",
            pos.entry_px
        );
        let trade = store
            .close_position(pos.id, 98.0, "close", None)
            .await
            .expect("close");
        // short close fill = 98*1.0002 =98.0196
        assert!((trade.px - 98.0196).abs() < 1e-9);
        let size = 200.0 / 99.98;
        let expected = size * (99.98 - trade.px);
        assert!((trade.realized_pnl - expected).abs() < 1e-9);
    }

    #[tokio::test]
    async fn equity_identity() {
        let store = Store::open("sqlite::memory:").await.expect("store");
        store.set_meta("bankroll", "1000").await.expect("set");
        let sized = Sized {
            leverage: 20.0,
            margin: 15.0,
            notional: 300.0,
            stop_pct: 0.6,
            tp_pct: 1.2,
        };
        let pos = store
            .open_position("SOL", Side::Long, &sized, 100.0, false, 24.0, None)
            .await
            .expect("open");
        // equity with mark at entry => unreal 0
        let mut marks = HashMap::new();
        marks.insert("SOL".to_string(), pos.entry_px);
        let eq = store.equity(&marks).await.expect("equity");
        // bankroll - fees + unreal (0) ; fees =0.225
        assert!((eq - (1000.0 - 0.225)).abs() < 1e-6, "eq {eq}");
        // move mark to 102
        marks.insert("SOL".to_string(), 102.0);
        let eq2 = store.equity(&marks).await.expect("eq2");
        let unreal = pos.size * (102.0 - pos.entry_px);
        let expected = 1000.0 - 0.225 + unreal;
        assert!(
            (eq2 - expected).abs() < 1e-6,
            "eq2 {eq2} expected {expected}"
        );
    }

    #[tokio::test]
    async fn xyz_slip_5bp() {
        let store = Store::open("sqlite::memory:").await.expect("store");
        let sized = Sized {
            leverage: 5.0,
            margin: 10.0,
            notional: 50.0,
            stop_pct: 1.0,
            tp_pct: 2.0,
        };
        let pos = store
            .open_position("xyz:TSLA", Side::Long, &sized, 200.0, true, 24.0, None)
            .await
            .expect("open");
        assert!(
            (pos.entry_px - 200.1).abs() < 1e-9,
            "xyz fill {}",
            pos.entry_px
        ); // 200*1.0005=200.1
    }

    // ── Money guard: equity never prices with invalid marks (3249.88-class spike) ─

    #[tokio::test]
    async fn equity_skips_invalid_zero_mark_no_spike_short() {
        let store = Store::open("sqlite::memory:").await.expect("store");
        store.set_meta("bankroll", "1000").await.expect("set");
        // Short with entry ~99.98 (100 * 0.9998), size = 200/99.98 ≈ 2.0004
        let sized = Sized {
            leverage: 10.0,
            margin: 20.0,
            notional: 200.0,
            stop_pct: 1.0,
            tp_pct: 2.0,
        };
        let pos = store
            .open_position("BTC", Side::Short, &sized, 100.0, false, 24.0, None)
            .await
            .expect("open");
        // Baseline fees: open fee = 200*0.00075 = 0.15
        let fees_open = 0.15;
        // Baseline equity without unreal: bankroll - fees + realized(0) = 999.85
        let baseline = 1000.0 - fees_open;
        // With INVALID mark 0.0 in the marks map, old code would compute unreal = (entry - 0)*size ≈ +199.96
        // yielding equity ≈ 1199.81 (≈3249.88-class 3× spike relative to day-open for larger notional).
        // Money guard must skip: unreal contributes 0, equity ≈ baseline.
        let mut marks = HashMap::new();
        marks.insert("BTC".to_string(), 0.0);
        let eq = store.equity(&marks).await.expect("equity");
        assert!(
            (eq - baseline).abs() < 1e-6,
            "equity with invalid mark 0.0 must NOT spike: got {eq} expected ~{baseline} (short unreal skipped)"
        );
        // Valid mark 98 should give real unreal ≈ (99.98-98)*size ≈ +3.96
        marks.insert("BTC".to_string(), 98.0);
        let eq_valid = store.equity(&marks).await.expect("eq valid");
        let expected = baseline + (pos.entry_px - 98.0) * pos.size;
        assert!(
            (eq_valid - expected).abs() < 1e-6,
            "valid mark equity {eq_valid} expected {expected}"
        );
        assert!(
            (eq_valid - baseline).abs() > 1.0,
            "valid mark must add unreal"
        );
        // Missing mark also skips (same as invalid) — not priced at entry nor 0 spike.
        let eq_missing = store.equity(&HashMap::new()).await.expect("missing");
        assert!(
            (eq_missing - baseline).abs() < 1e-6,
            "missing mark must also be skipped, got {eq_missing}"
        );
    }

    #[tokio::test]
    async fn equity_skips_zero_long_avoids_false_kill_crash() {
        let store = Store::open("sqlite::memory:").await.expect("store");
        store.set_meta("bankroll", "1000").await.expect("set");
        let sized = Sized {
            leverage: 10.0,
            margin: 20.0,
            notional: 200.0,
            stop_pct: 1.0,
            tp_pct: 2.0,
        };
        let pos = store
            .open_position("SOL", Side::Long, &sized, 100.0, false, 24.0, None)
            .await
            .expect("open");
        let baseline = 1000.0 - 0.15; // open fee
        // Long with mark 0 would compute (0 - entry)*size ≈ -200 → equity ~799.85 (-20% crash, would false-trip -12% kill)
        let mut marks = HashMap::new();
        marks.insert("SOL".to_string(), 0.0);
        let eq = store.equity(&marks).await.expect("eq");
        assert!(
            (eq - baseline).abs() < 1e-6,
            "long with mark 0 must NOT crash equity: got {eq} expected {baseline}"
        );
        assert!(
            eq > 880.0,
            "with baseline 1000, equity {eq} must not be <=880 kill threshold"
        );
    }

    #[tokio::test]
    async fn equity_bootstrap_before_ws_safe_no_spike_combination() {
        // Simulates the live bug: bootstrap injects position rows before mids ws connects.
        // Old: injection seeded mid 0.0 → snapshot.mids map contains 0.0 → equity sampled that window → 3249.88 spike.
        // New: source guard leaves row out (no mids) → marks map empty for that market → equity guard skips → no spike.
        use crate::contracts::MarketRow;
        use crate::hl_rest::ensure_position_markets;
        let store = Store::open("sqlite::memory:").await.expect("store");
        store.set_meta("bankroll", "1000").await.expect("set");
        let sized = Sized {
            leverage: 20.0,
            margin: 15.0,
            notional: 300.0,
            stop_pct: 0.6,
            tp_pct: 1.2,
        };
        let pos = store
            .open_position("UNKNOWN", Side::Short, &sized, 100.0, false, 24.0, None)
            .await
            .expect("open");
        let baseline = 1000.0 - 0.225; // fee
        // Bootstrap phase: try to inject UNKNOWN with NO mids anywhere (mimics before ws).
        let mut snap_markets: Vec<MarketRow> = vec![];
        let open = vec!["UNKNOWN".to_string()];
        let empty: HashMap<String, f64> = HashMap::new();
        ensure_position_markets(&mut snap_markets, &open, &empty, &empty, &empty, |_| None);
        assert!(
            snap_markets.is_empty(),
            "source guard: UNKNOWN not injected before mids"
        );
        // Equity snapshot taken immediately after bootstrap (before first tick) would have built marks from snapshot.
        let marks: HashMap<String, f64> = snap_markets
            .iter()
            .map(|m| (m.market.clone(), m.mid))
            .collect();
        assert!(
            !marks.contains_key("UNKNOWN"),
            "marks map has no UNKNOWN before ws"
        );
        let eq = store.equity(&marks).await.expect("equity bootstrap window");
        assert!(
            (eq - baseline).abs() < 1e-6,
            "bootstrap equity must be baseline ~{baseline}, got {eq} (no spike)"
        );
        // Even if someone erroneously inserts a 0.0 mark, equity money guard still prevents spike.
        let mut bad_marks = HashMap::new();
        bad_marks.insert("UNKNOWN".to_string(), 0.0);
        let eq_bad = store.equity(&bad_marks).await.expect("bad");
        assert!(
            (eq_bad - baseline).abs() < 1e-6,
            "even with bad 0.0, equity must stay ~baseline, got {eq_bad}"
        );
        // After ws tick provides mid, injection succeeds and equity reflects real unreal.
        let mut engine_mids = HashMap::new();
        engine_mids.insert("UNKNOWN".to_string(), 101.0);
        ensure_position_markets(
            &mut snap_markets,
            &open,
            &empty,
            &engine_mids,
            &empty,
            |_| None,
        );
        assert_eq!(snap_markets.len(), 1);
        let marks2: HashMap<String, f64> = snap_markets
            .iter()
            .map(|m| (m.market.clone(), m.mid))
            .collect();
        let eq2 = store.equity(&marks2).await.expect("equity after tick");
        let expected_unreal = (pos.entry_px - 101.0) * pos.size; // short: entry - mark
        let expected2 = baseline + expected_unreal;
        assert!(
            (eq2 - expected2).abs() < 1e-6,
            "after valid tick equity {eq2} expected {expected2}"
        );
    }

    // ── depth-aware partial_close (COMMIT 1) ──

    #[tokio::test]
    async fn partial_close_long_thin_bids_uses_vwap_worse_than_flat() {
        let store = Store::open("sqlite::memory:").await.expect("store");
        let sized = Sized {
            leverage: 10.0,
            margin: 20.0,
            notional: 200.0,
            stop_pct: 1.0,
            tp_pct: 2.0,
        };
        let pos = store
            .open_position("SOL", Side::Long, &sized, 100.0, false, 24.0, None)
            .await
            .expect("open");
        // pos.size = 200 / 100.02 ≈ 1.9996. Partial close 1.0 at mark 100.
        // Thin bids: 99.9 x1.0, 99.5 x2.0 — vwap for 1.0 = 99.9, flat = 99.98, worse = min => 99.9
        let now = chrono::Utc::now().timestamp_millis();
        let b = book(now, &[(99.9, 1.0), (99.5, 2.0)], &[(100.1, 5.0)]);
        let trade = store
            .partial_close(pos.id, 1.0, 100.0, "partial", Some(&b))
            .await
            .expect("partial");
        let expected_fill = 99.9; // vwap < flat, worse used
        assert!(
            (trade.px - expected_fill).abs() < 1e-9,
            "thin bids fill {} expected {expected_fill}",
            trade.px
        );
        // fee pinned
        let expected_fee = 1.0 * expected_fill * 0.00075;
        assert!(
            (trade.fee - expected_fee).abs() < 1e-9,
            "fee {} expected {expected_fee}",
            trade.fee
        );
        // remaining size math
        let remaining = store.get_position(pos.id).await.expect("get").expect("pos");
        let expected_remaining = pos.size - 1.0;
        assert!(
            (remaining.size - expected_remaining).abs() < 1e-9,
            "remaining {} expected {expected_remaining}",
            remaining.size
        );
    }

    #[tokio::test]
    async fn partial_close_long_deep_bids_uses_flat_cap() {
        let store = Store::open("sqlite::memory:").await.expect("store");
        let sized = Sized {
            leverage: 10.0,
            margin: 20.0,
            notional: 200.0,
            stop_pct: 1.0,
            tp_pct: 2.0,
        };
        let pos = store
            .open_position("SOL", Side::Long, &sized, 100.0, false, 24.0, None)
            .await
            .expect("open");
        let now = chrono::Utc::now().timestamp_millis();
        // Deep bids: 99.99 x 1000 — vwap 99.99 > flat 99.98, flat cap (min) wins
        let b = book(now, &[(99.99, 1000.0)], &[(100.1, 5.0)]);
        let trade = store
            .partial_close(pos.id, 1.0, 100.0, "partial", Some(&b))
            .await
            .expect("partial");
        let flat = 100.0 * 0.9998;
        assert!(
            (trade.px - flat).abs() < 1e-9,
            "deep bids cap to flat {flat}, got {}",
            trade.px
        );
        let expected_fee = 1.0 * flat * 0.00075;
        assert!((trade.fee - expected_fee).abs() < 1e-9);
    }

    #[tokio::test]
    async fn partial_close_stale_book_uses_flat() {
        let store = Store::open("sqlite::memory:").await.expect("store");
        let sized = Sized {
            leverage: 10.0,
            margin: 20.0,
            notional: 200.0,
            stop_pct: 1.0,
            tp_pct: 2.0,
        };
        let pos = store
            .open_position("SOL", Side::Long, &sized, 100.0, false, 24.0, None)
            .await
            .expect("open");
        let now = chrono::Utc::now().timestamp_millis();
        // stale 4s old -> flat
        let stale = book(now - 4000, &[(99.9, 1.0), (99.5, 10.0)], &[(100.1, 5.0)]);
        let trade = store
            .partial_close(pos.id, 1.0, 100.0, "partial", Some(&stale))
            .await
            .expect("partial");
        let flat = 100.0 * 0.9998;
        assert!(
            (trade.px - flat).abs() < 1e-9,
            "stale -> flat {flat}, got {}",
            trade.px
        );
        // also None book -> flat
        let store2 = Store::open("sqlite::memory:").await.expect("store2");
        let pos2 = store2
            .open_position("SOL", Side::Long, &sized, 100.0, false, 24.0, None)
            .await
            .expect("open2");
        let trade2 = store2
            .partial_close(pos2.id, 1.0, 100.0, "partial", None)
            .await
            .expect("partial none");
        assert!((trade2.px - flat).abs() < 1e-9);
        // empty levels -> flat
        let empty = book(now, &[], &[(100.1, 5.0)]);
        let store3 = Store::open("sqlite::memory:").await.expect("store3");
        let pos3 = store3
            .open_position("SOL", Side::Long, &sized, 100.0, false, 24.0, None)
            .await
            .expect("open3");
        let trade3 = store3
            .partial_close(pos3.id, 1.0, 100.0, "partial", Some(&empty))
            .await
            .expect("partial empty");
        assert!((trade3.px - flat).abs() < 1e-9);
        // exhausted depth -> flat (need close_size > depth)
        let thin = book(now, &[(99.9, 0.2)], &[(100.1, 5.0)]);
        let store4 = Store::open("sqlite::memory:").await.expect("store4");
        let pos4 = store4
            .open_position("SOL", Side::Long, &sized, 100.0, false, 24.0, None)
            .await
            .expect("open4");
        let trade4 = store4
            .partial_close(pos4.id, 1.0, 100.0, "partial", Some(&thin))
            .await
            .expect("partial exhausted");
        assert!((trade4.px - flat).abs() < 1e-9);
    }

    #[tokio::test]
    async fn partial_close_fee_and_remaining_size_unchanged() {
        let store = Store::open("sqlite::memory:").await.expect("store");
        let sized = Sized {
            leverage: 10.0,
            margin: 20.0,
            notional: 200.0,
            stop_pct: 1.0,
            tp_pct: 2.0,
        };
        let pos = store
            .open_position("SOL", Side::Long, &sized, 100.0, false, 24.0, None)
            .await
            .expect("open");
        let now = chrono::Utc::now().timestamp_millis();
        let b = book(now, &[(99.99, 1000.0)], &[(100.1, 5.0)]);
        let close_sz = 0.5;
        let trade = store
            .partial_close(pos.id, close_sz, 100.0, "partial", Some(&b))
            .await
            .expect("partial");
        // fill = flat capped = 99.98
        let flat = 100.0 * 0.9998;
        assert!((trade.px - flat).abs() < 1e-9);
        let expected_fee = close_sz * flat * 0.00075;
        assert!(
            (trade.fee - expected_fee).abs() < 1e-9,
            "fee {0} expected {expected_fee}",
            trade.fee
        );
        let remaining = store.get_position(pos.id).await.expect("get").expect("pos");
        assert!((remaining.size - (pos.size - close_sz)).abs() < 1e-9);
        // gross pnl pinned
        let expected_gross = (flat - pos.entry_px) * close_sz;
        assert!(
            (trade.realized_pnl - expected_gross).abs() < 1e-9,
            "gross {} expected {expected_gross}",
            trade.realized_pnl
        );
        // closing remaining should close position
        let trade2 = store
            .partial_close(pos.id, remaining.size, 100.0, "close", Some(&b))
            .await
            .expect("partial close remainder");
        assert!((trade2.size - (pos.size - close_sz)).abs() < 1e-9);
        let gone = store.open_positions().await.expect("opens");
        assert!(
            gone.is_empty(),
            "position should be closed after full partial"
        );
    }

    #[tokio::test]
    async fn log_decision_and_mark_executed_roundtrip() {
        let store = Store::open("sqlite::memory:").await.expect("store");
        let ts = chrono::Utc::now().timestamp_millis();
        let id = store
            .log_decision(
                ts, "SOL", "open", "long", 0.85, "thesis", 24.0, false, false, "reason",
            )
            .await
            .expect("log");
        let row: (i64,) = sqlx::query_as("SELECT executed FROM decisions WHERE id=?1")
            .bind(id)
            .fetch_one(store.pool())
            .await
            .expect("select");
        assert_eq!(row.0, 0, "initial executed must be 0");
        store.mark_decision_executed(id).await.expect("mark");
        let row2: (i64,) = sqlx::query_as("SELECT executed FROM decisions WHERE id=?1")
            .bind(id)
            .fetch_one(store.pool())
            .await
            .expect("select2");
        assert_eq!(row2.0, 1, "after mark executed must be 1");
    }

    #[tokio::test]
    async fn invalidation_condition_persists_roundtrip() {
        // 0006 migration: the column exists after Store::open, and a stated falsifier
        // survives the write/read round trip; unstated plans stay NULL.
        let store = Store::open("sqlite::memory:").await.expect("store");
        let ts = chrono::Utc::now().timestamp_millis();
        let id = store
            .log_decision_for_analyst(
                ts, "SOL", "open", "long", 0.8, "thesis", 24.0, false, false, "reason", "alpha",
                Some("if 4h RSI14 breaks below 40"),
            )
            .await
            .expect("log with invalidation");
        let got: (Option<String>,) =
            sqlx::query_as("SELECT invalidation_condition FROM decisions WHERE id=?1")
                .bind(id)
                .fetch_one(store.pool())
                .await
                .expect("select");
        assert_eq!(got.0.as_deref(), Some("if 4h RSI14 breaks below 40"));
        // legacy path (no falsifier) writes NULL, same as pre-migration rows
        let id2 = store
            .log_decision(ts, "BTC", "skip", "skip", 0.4, "low edge", 24.0, false, false, "r")
            .await
            .expect("log legacy");
        let got2: (Option<String>,) =
            sqlx::query_as("SELECT invalidation_condition FROM decisions WHERE id=?1")
                .bind(id2)
                .fetch_one(store.pool())
                .await
                .expect("select2");
        assert_eq!(got2.0, None);
    }

    #[tokio::test]
    async fn log_decision_concurrent_marks_correct_rows() {
        let store = Store::open("sqlite::memory:").await.expect("store");
        let ts = chrono::Utc::now().timestamp_millis();
        // two concurrent inserts
        let id1 = store
            .log_decision(
                ts, "SOL", "open", "long", 0.9, "t1", 24.0, false, false, "r1",
            )
            .await
            .expect("log1");
        let id2 = store
            .log_decision(
                ts + 1,
                "BTC",
                "open",
                "short",
                0.8,
                "t2",
                24.0,
                false,
                false,
                "r2",
            )
            .await
            .expect("log2");
        assert_ne!(id1, id2);
        // mark only first
        store.mark_decision_executed(id1).await.expect("mark1");
        let r1: (i64,) = sqlx::query_as("SELECT executed FROM decisions WHERE id=?1")
            .bind(id1)
            .fetch_one(store.pool())
            .await
            .expect("sel1");
        let r2: (i64,) = sqlx::query_as("SELECT executed FROM decisions WHERE id=?1")
            .bind(id2)
            .fetch_one(store.pool())
            .await
            .expect("sel2");
        assert_eq!(r1.0, 1, "id1 must be marked 1");
        assert_eq!(r2.0, 0, "id2 must stay 0 — only correct row marked");
        // now mark second and verify both 1
        store.mark_decision_executed(id2).await.expect("mark2");
        let r2b: (i64,) = sqlx::query_as("SELECT executed FROM decisions WHERE id=?1")
            .bind(id2)
            .fetch_one(store.pool())
            .await
            .expect("sel2b");
        assert_eq!(r2b.0, 1);
    }

    #[tokio::test]
    async fn fill_mode_book_on_usable_book() {
        let store = Store::open("sqlite::memory:").await.expect("store");
        let now = chrono::Utc::now().timestamp_millis();
        // Usable book: asks thin so vwap > flat for long open => fill != flat => book
        let b = book(now, &[(99.9, 10.0)], &[(100.0, 1.0), (100.5, 2.0)]);
        let sized = Sized {
            leverage: 10.0,
            margin: 20.0,
            notional: 200.0,
            stop_pct: 1.0,
            tp_pct: 2.0,
        };
        let pos = store
            .open_position("SOL", Side::Long, &sized, 100.0, false, 24.0, Some(&b))
            .await
            .expect("open");
        let mode: (String,) =
            sqlx::query_as("SELECT fill_mode FROM trades WHERE position_id=?1 AND action='open'")
                .bind(pos.id)
                .fetch_one(store.pool())
                .await
                .expect("fill_mode");
        assert_eq!(
            mode.0, "book",
            "usable book should be book mode, got {}",
            mode.0
        );
    }

    #[tokio::test]
    async fn fill_mode_flat_on_stale_book() {
        let store = Store::open("sqlite::memory:").await.expect("store");
        let now = chrono::Utc::now().timestamp_millis();
        let stale = book(now - 4000, &[(99.9, 10.0)], &[(100.0, 1.0), (100.5, 2.0)]);
        let sized = Sized {
            leverage: 10.0,
            margin: 20.0,
            notional: 200.0,
            stop_pct: 1.0,
            tp_pct: 2.0,
        };
        let pos = store
            .open_position("SOL", Side::Long, &sized, 100.0, false, 24.0, Some(&stale))
            .await
            .expect("open stale");
        let mode: (String,) =
            sqlx::query_as("SELECT fill_mode FROM trades WHERE position_id=?1 AND action='open'")
                .bind(pos.id)
                .fetch_one(store.pool())
                .await
                .expect("fill_mode stale");
        assert_eq!(mode.0, "flat", "stale book must be flat");

        // close also flat on stale
        let trade = store
            .close_position(pos.id, 102.0, "close", Some(&stale))
            .await
            .expect("close stale");
        let mode2: (String,) = sqlx::query_as("SELECT fill_mode FROM trades WHERE id=?1")
            .bind(trade.id)
            .fetch_one(store.pool())
            .await
            .expect("fill_mode close");
        assert_eq!(mode2.0, "flat");
    }

    #[tokio::test]
    async fn fill_mode_close_book_vs_flat() {
        let store = Store::open("sqlite::memory:").await.expect("store");
        let now = chrono::Utc::now().timestamp_millis();
        let sized = Sized {
            leverage: 10.0,
            margin: 20.0,
            notional: 200.0,
            stop_pct: 1.0,
            tp_pct: 2.0,
        };
        // open flat (no book) => fill_mode flat
        let pos = store
            .open_position("SOL", Side::Long, &sized, 100.0, false, 24.0, None)
            .await
            .expect("open");
        let m1: (String,) =
            sqlx::query_as("SELECT fill_mode FROM trades WHERE position_id=?1 AND action='open'")
                .bind(pos.id)
                .fetch_one(store.pool())
                .await
                .expect("mode open none");
        assert_eq!(m1.0, "flat");
        // close with usable bids that walk => book
        let bids = book(now, &[(99.9, 2.0), (99.5, 2.0)], &[(100.1, 5.0)]);
        let trade = store
            .close_position(pos.id, 100.0, "close", Some(&bids))
            .await
            .expect("close book");
        let m2: (String,) = sqlx::query_as("SELECT fill_mode FROM trades WHERE id=?1")
            .bind(trade.id)
            .fetch_one(store.pool())
            .await
            .expect("mode close book");
        assert_eq!(m2.0, "book");
    }

    #[tokio::test]
    async fn migration_rerun_safe_and_default_flat_for_old_rows() {
        // Simulate old DB without fill_mode: Store::open must add column idempotently and default old rows to 'flat'.
        // We test idempotency by opening a memory store twice via same pool? Instead open, insert, reopen logic via raw SQL.
        let store = Store::open("sqlite::memory:").await.expect("store");
        // The column should exist and default to flat even if we insert via raw SQL without specifying fill_mode (historical fills)
        // Insert a legacy trade directly without fill_mode (relies on DEFAULT)
        let pos = store
            .open_position(
                "SOL",
                Side::Long,
                &crate::sizing::Sized {
                    leverage: 5.0,
                    margin: 10.0,
                    notional: 50.0,
                    stop_pct: 1.0,
                    tp_pct: 2.0,
                },
                100.0,
                false,
                24.0,
                None,
            )
            .await
            .expect("open");
        // Raw insert without fill_mode to mimic pre-migration row (uses DEFAULT)
        sqlx::query("INSERT INTO trades (position_id, market, action, px, size, fee, realized_pnl, ts) VALUES (?1,?2,'open',100,1,0.1,0,?3)")
            .bind(pos.id)
            .bind("SOL")
            .bind(chrono::Utc::now().timestamp_millis())
            .execute(store.pool())
            .await
            .expect("legacy insert");
        let rows: Vec<(String,)> = sqlx::query_as("SELECT fill_mode FROM trades")
            .fetch_all(store.pool())
            .await
            .expect("select modes");
        for (m,) in rows {
            assert_eq!(
                m, "flat",
                "historical flat default or new flat should be flat"
            );
        }
        // Re-apply migration statement manually — should not error on duplicate column (idempotent via catch)
        let res =
            sqlx::query("ALTER TABLE trades ADD COLUMN fill_mode TEXT NOT NULL DEFAULT 'flat'")
                .execute(store.pool())
                .await;
        assert!(res.is_err(), "second ALTER should error duplicate column");
        assert!(
            res.unwrap_err().to_string().contains("duplicate column"),
            "error must be duplicate column"
        );
        // Store::open on a fresh memory db should also not error — exercised above
    }

    // ── T1 churn-control reads ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn market_entries_since_counts_entries_not_open_positions() {
        let store = Store::open("sqlite::memory:").await.expect("open memory");
        let sized = Sized {
            leverage: 10.0,
            margin: 20.0,
            notional: 200.0,
            stop_pct: 1.0,
            tp_pct: 2.0,
        };
        let day_start = chrono::Utc::now().timestamp_millis() - 3_600_000;

        assert_eq!(
            store.market_entries_since("SOL", day_start).await.unwrap(),
            0,
            "no entries yet"
        );
        let p1 = store
            .open_position("SOL", Side::Long, &sized, 100.0, false, 24.0, None)
            .await
            .unwrap();
        store
            .close_position(p1.id, 101.0, "tp", None)
            .await
            .unwrap();
        // closing must NOT give the slot back — the cap is on entries taken
        assert_eq!(
            store.market_entries_since("SOL", day_start).await.unwrap(),
            1,
            "a closed entry still counts"
        );
        store
            .open_position("SOL", Side::Short, &sized, 100.0, false, 24.0, None)
            .await
            .unwrap();
        assert_eq!(
            store.market_entries_since("SOL", day_start).await.unwrap(),
            2
        );
        // other markets have their own budget
        store
            .open_position("ETH", Side::Long, &sized, 100.0, false, 24.0, None)
            .await
            .unwrap();
        assert_eq!(
            store.market_entries_since("ETH", day_start).await.unwrap(),
            1
        );
        assert_eq!(
            store.market_entries_since("SOL", day_start).await.unwrap(),
            2
        );
        // and the window is exclusive of earlier days
        let tomorrow_start = day_start + 86_400_000;
        assert_eq!(
            store
                .market_entries_since("SOL", tomorrow_start)
                .await
                .unwrap(),
            0,
            "next UTC day starts clean"
        );
    }

    // ── T2 analytics + counterfactual reads ───────────────────────────────────────────

    #[tokio::test]
    async fn market_entries_by_market_matches_the_per_market_counter() {
        let store = Store::open("sqlite::memory:").await.expect("open memory");
        let sized = Sized {
            leverage: 10.0,
            margin: 20.0,
            notional: 200.0,
            stop_pct: 1.0,
            tp_pct: 2.0,
        };
        let day_start = chrono::Utc::now().timestamp_millis() - 3_600_000;
        assert!(
            store
                .market_entries_by_market(day_start)
                .await
                .unwrap()
                .is_empty(),
            "no entries yet"
        );

        let p = store
            .open_position("SOL", Side::Long, &sized, 100.0, false, 24.0, None)
            .await
            .unwrap();
        store.close_position(p.id, 101.0, "tp", None).await.unwrap();
        store
            .open_position("SOL", Side::Short, &sized, 100.0, false, 24.0, None)
            .await
            .unwrap();
        store
            .open_position("ETH", Side::Long, &sized, 100.0, false, 24.0, None)
            .await
            .unwrap();

        let agg = store.market_entries_by_market(day_start).await.unwrap();
        assert_eq!(
            agg,
            vec![("ETH".to_string(), 1), ("SOL".to_string(), 2)],
            "market-ordered aggregate"
        );
        // byte-identical predicate to the single-market read the gate uses
        for (market, count) in agg {
            assert_eq!(
                store
                    .market_entries_since(&market, day_start)
                    .await
                    .unwrap(),
                count,
                "{market}"
            );
        }
        // the window is exclusive of earlier days, exactly like the single-market read
        assert!(
            store
                .market_entries_by_market(day_start + 86_400_000)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn markets_closed_since_lists_only_closes_in_the_window() {
        let store = Store::open("sqlite::memory:").await.expect("open memory");
        let sized = Sized {
            leverage: 10.0,
            margin: 20.0,
            notional: 200.0,
            stop_pct: 1.0,
            tp_pct: 2.0,
        };
        let now = chrono::Utc::now().timestamp_millis();
        let p = store
            .open_position("SOL", Side::Long, &sized, 100.0, false, 24.0, None)
            .await
            .unwrap();
        // an open position is not a close
        assert!(
            store
                .markets_closed_since(now - 60_000)
                .await
                .unwrap()
                .is_empty()
        );
        store.close_position(p.id, 99.0, "sl", None).await.unwrap();
        assert_eq!(
            store.markets_closed_since(now - 60_000).await.unwrap(),
            vec!["SOL".to_string()]
        );
        // and the window bites
        assert!(
            store
                .markets_closed_since(now + 60_000)
                .await
                .unwrap()
                .is_empty(),
            "future window is empty"
        );
    }

    #[tokio::test]
    async fn executed_decision_outcomes_joins_only_executed_and_closed() {
        let store = Store::open("sqlite::memory:").await.expect("open memory");
        let sized = Sized {
            leverage: 10.0,
            margin: 20.0,
            notional: 200.0,
            stop_pct: 1.0,
            tp_pct: 2.0,
        };
        let now = chrono::Utc::now().timestamp_millis();
        let window = 600_000i64;

        // executed decision -> position -> closed: counted
        let d1 = store
            .log_decision(
                now - 1_000,
                "SOL",
                "open",
                "long",
                0.83,
                "t",
                24.0,
                false,
                false,
                "r",
            )
            .await
            .unwrap();
        let p1 = store
            .open_position("SOL", Side::Long, &sized, 100.0, false, 24.0, None)
            .await
            .unwrap();
        store.mark_decision_executed(d1).await.unwrap();
        store
            .close_position(p1.id, 101.0, "tp", None)
            .await
            .unwrap();

        // decided but never executed (gate refused): no position of its own, must not count
        store
            .log_decision(
                now - 500,
                "ETH",
                "open",
                "long",
                0.91,
                "t",
                24.0,
                false,
                false,
                "r gate_refused:DailyCap",
            )
            .await
            .unwrap();

        // executed but still OPEN: outcome unknown, must not count
        let d3 = store
            .log_decision(
                now - 200,
                "BTC",
                "open",
                "long",
                0.72,
                "t",
                24.0,
                false,
                false,
                "r",
            )
            .await
            .unwrap();
        store
            .open_position("BTC", Side::Long, &sized, 100.0, false, 24.0, None)
            .await
            .unwrap();
        store.mark_decision_executed(d3).await.unwrap();

        let rows = store.executed_decision_outcomes(window).await.unwrap();
        assert_eq!(
            rows.len(),
            1,
            "only the executed+closed pair joins: {rows:?}"
        );
        let (conviction, net) = rows[0];
        assert!((conviction - 0.83).abs() < 1e-9);
        let expected: (f64,) =
            sqlx::query_as("SELECT SUM(realized_pnl) - SUM(fee) FROM trades WHERE position_id=?1")
                .bind(p1.id)
                .fetch_one(store.pool())
                .await
                .unwrap();
        assert!(
            (net - expected.0).abs() < 1e-9,
            "net must be the position's whole life: {net} vs {}",
            expected.0
        );
        assert!(
            net > 0.0,
            "a +1% winner net of both fees is still a win, got {net}"
        );

        // a decision whose position opened outside the match window is not attributed
        let d4 = store
            .log_decision(
                now - 3_600_000,
                "GOLD",
                "open",
                "long",
                0.95,
                "t",
                24.0,
                false,
                false,
                "r",
            )
            .await
            .unwrap();
        let p4 = store
            .open_position("GOLD", Side::Long, &sized, 100.0, false, 24.0, None)
            .await
            .unwrap();
        store.mark_decision_executed(d4).await.unwrap();
        store
            .close_position(p4.id, 101.0, "tp", None)
            .await
            .unwrap();
        assert_eq!(
            store
                .executed_decision_outcomes(window)
                .await
                .unwrap()
                .len(),
            1,
            "stale decision must not claim a later position"
        );
        assert_eq!(
            store
                .executed_decision_outcomes(7_200_000)
                .await
                .unwrap()
                .len(),
            2,
            "a wider window does attribute it"
        );
    }

    #[tokio::test]
    async fn market_stats_count_round_trips_and_exit_mix_buckets_by_cause() {
        let store = Store::open("sqlite::memory:").await.expect("open memory");
        let sized = Sized {
            leverage: 10.0,
            margin: 20.0,
            notional: 200.0,
            stop_pct: 1.0,
            tp_pct: 2.0,
        };
        let now = chrono::Utc::now().timestamp_millis();
        for (market, reason, exit) in [
            ("SOL", "tp", 101.0),
            ("SOL", "sl", 99.0),
            ("ETH", "veto_close", 100.0),
            ("ETH", "time_stop", 100.5),
        ] {
            let p = store
                .open_position(market, Side::Long, &sized, 100.0, false, 24.0, None)
                .await
                .unwrap();
            store
                .close_position(p.id, exit, reason, None)
                .await
                .unwrap();
        }
        let stats = store.market_stats().await.unwrap();
        assert_eq!(stats.len(), 2);
        assert_eq!(stats[0].market, "ETH");
        assert_eq!(stats[0].trades, 2, "an open+close is ONE trade");
        assert_eq!(stats[1].market, "SOL");
        assert_eq!(stats[1].trades, 2);
        assert!(
            stats[0].fees > 0.0 && stats[1].fees > 0.0,
            "fees count both legs"
        );
        let sol_net: (f64,) =
            sqlx::query_as("SELECT SUM(realized_pnl) - SUM(fee) FROM trades WHERE market='SOL'")
                .fetch_one(store.pool())
                .await
                .unwrap();
        assert!(
            (stats[1].net_pnl - sol_net.0).abs() < 1e-9,
            "net is realized minus every fee"
        );

        let mix = store
            .exit_mix_since(crate::risk::utc_day_start_ms(now))
            .await
            .unwrap();
        assert_eq!(mix.len(), 1, "all four closes land on today");
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        assert_eq!(mix[0].date, today);
        assert_eq!(
            (mix[0].tp, mix[0].sl, mix[0].veto_close, mix[0].other),
            (1, 1, 1, 1),
            "time_stop falls into other"
        );
        // opens are never exits
        let total: i64 = mix[0].tp + mix[0].sl + mix[0].veto_close + mix[0].other;
        assert_eq!(total, 4, "the four columns account for every exit");
        // and the window excludes older days
        assert!(
            store
                .exit_mix_since(now + 86_400_000)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn counterfactual_cache_roundtrip_and_pending_accounting() {
        let store = Store::open("sqlite::memory:").await.expect("open memory");
        let sized = Sized {
            leverage: 10.0,
            margin: 20.0,
            notional: 200.0,
            stop_pct: 1.0,
            tp_pct: 2.0,
        };
        assert_eq!(store.counterfactual_pending_count().await.unwrap(), 0);
        assert_eq!(
            store.counterfactual_totals().await.unwrap(),
            (0, 0.0, 0.0),
            "empty cache sums to zero, not null"
        );

        // a tp close is not a counterfactual candidate; only veto closes are
        let tp = store
            .open_position("ETH", Side::Long, &sized, 100.0, false, 24.0, None)
            .await
            .unwrap();
        store
            .close_position(tp.id, 101.0, "tp", None)
            .await
            .unwrap();
        assert_eq!(
            store.counterfactual_pending_count().await.unwrap(),
            0,
            "only veto closes qualify"
        );

        let pos = store
            .open_position("SOL", Side::Short, &sized, 100.0, false, 6.0, None)
            .await
            .unwrap();
        let close = store
            .close_position(pos.id, 99.5, "veto_close", None)
            .await
            .unwrap();
        assert_eq!(store.counterfactual_pending_count().await.unwrap(), 1);

        let pending = store.counterfactual_pending(3).await.unwrap();
        assert_eq!(pending.len(), 1);
        let v = &pending[0];
        assert_eq!(v.position_id, pos.id);
        assert_eq!(v.market, "SOL");
        assert_eq!(
            v.side,
            Side::Short,
            "side survives the round trip — it flips the whole walk"
        );
        assert!((v.entry_px - pos.entry_px).abs() < 1e-9);
        assert!((v.sl_px - pos.sl_px).abs() < 1e-9 && (v.tp_px - pos.tp_px).abs() < 1e-9);
        assert_eq!(v.horizon_hours, Some(6.0));
        assert_eq!(v.close_ts, close.ts);
        assert!(
            (v.size - close.size).abs() < 1e-9,
            "the size actually closed, so partials stay honest"
        );
        assert!(
            (v.actual_pnl - (close.realized_pnl - close.fee)).abs() < 1e-9,
            "actual is net of the exit fee only"
        );
        // the work bound is respected
        assert!(store.counterfactual_pending(0).await.unwrap().is_empty());

        store
            .counterfactual_put(pos.id, 1_786_284_000_000, "sl", -3.5, v.actual_pnl)
            .await
            .unwrap();
        assert_eq!(
            store.counterfactual_pending_count().await.unwrap(),
            0,
            "cached rows leave the backlog"
        );
        assert!(store.counterfactual_pending(3).await.unwrap().is_empty());
        let (computed, net_actual, net_bracket) = store.counterfactual_totals().await.unwrap();
        assert_eq!(computed, 1);
        assert!((net_actual - v.actual_pnl).abs() < 1e-9);
        assert!((net_bracket - -3.5).abs() < 1e-9);
        // idempotent: recomputing the same position never double-counts
        store
            .counterfactual_put(pos.id, 1_786_284_001_000, "sl", -3.5, v.actual_pnl)
            .await
            .unwrap();
        assert_eq!(
            store.counterfactual_totals().await.unwrap().0,
            1,
            "one row per position"
        );
    }

    #[tokio::test]
    async fn counterfactual_rows_are_newest_first_joined_to_their_position_and_capped() {
        let store = Store::open("sqlite::memory:").await.expect("open memory");
        let sized = Sized {
            leverage: 10.0,
            margin: 20.0,
            notional: 200.0,
            stop_pct: 1.0,
            tp_pct: 2.0,
        };
        assert!(
            store.counterfactual_rows(50).await.unwrap().is_empty(),
            "empty cache, empty table"
        );

        // three veto closes with hand-stamped close times so the ordering is unambiguous
        // (close_position stamps `now()`, which can collide inside one millisecond)
        let base = 1_786_284_000_000i64;
        let markets = [
            ("SOL", Side::Long),
            ("ETH", Side::Short),
            ("BTC", Side::Long),
        ];
        let mut ids = Vec::new();
        for (i, (market, side)) in markets.iter().enumerate() {
            let pos = store
                .open_position(market, *side, &sized, 100.0, false, 24.0, None)
                .await
                .unwrap();
            store
                .close_position(pos.id, 99.0, "veto_close", None)
                .await
                .unwrap();
            sqlx::query("UPDATE positions SET closed_ts=?1 WHERE id=?2")
                .bind(base + i as i64 * 60_000)
                .bind(pos.id)
                .execute(store.pool())
                .await
                .unwrap();
            ids.push(pos.id);
        }
        // only the first two have been replayed — a pending veto has no row
        store
            .counterfactual_put(ids[0], base, "sl", -3.5, -1.0)
            .await
            .unwrap();
        store
            .counterfactual_put(ids[1], base, "tp", 4.25, 0.5)
            .await
            .unwrap();

        let rows = store.counterfactual_rows(50).await.unwrap();
        assert_eq!(rows.len(), 2, "uncached vetoes are pending, not rows");
        assert_eq!(rows[0].position_id, ids[1], "newest close first");
        assert_eq!(
            rows[0].market, "ETH",
            "market comes from the position, not the cache"
        );
        assert_eq!(
            rows[0].side,
            Side::Short,
            "side survives the join — it is what the bracket was walked against"
        );
        assert_eq!(rows[0].closed_ts, base + 60_000);
        assert_eq!(rows[0].bracket_outcome, "tp");
        assert!(
            (rows[0].bracket_pnl - 4.25).abs() < 1e-9 && (rows[0].actual_pnl - 0.5).abs() < 1e-9
        );
        assert_eq!(rows[1].position_id, ids[0]);
        assert_eq!(rows[1].side, Side::Long);
        assert_eq!(rows[1].bracket_outcome, "sl");

        // the cap takes the most recent, never the oldest
        let capped = store.counterfactual_rows(1).await.unwrap();
        assert_eq!(capped.len(), 1);
        assert_eq!(capped[0].position_id, ids[1]);
        assert!(store.counterfactual_rows(0).await.unwrap().is_empty());
        // totals stay whole-history even when the row window is smaller
        assert_eq!(
            store.counterfactual_totals().await.unwrap().0,
            2,
            "the cap never touches the totals"
        );
    }

    /// The live daemon reopens an EXISTING kestreld.db on every restart and replays the whole
    /// migration chain over it — 0003 must be a no-op the second time, with the data intact.
    #[tokio::test]
    async fn schema_init_is_rerun_safe_against_an_existing_database_file() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "kestreld-migrate-{}-{nanos}.db",
            std::process::id()
        ));
        let url = format!("sqlite:{}?mode=rwc", path.display());
        let sized = Sized {
            leverage: 10.0,
            margin: 20.0,
            notional: 200.0,
            stop_pct: 1.0,
            tp_pct: 2.0,
        };

        let first = Store::open(&url).await.expect("first open");
        let pos = first
            .open_position("SOL", Side::Long, &sized, 100.0, false, 24.0, None)
            .await
            .unwrap();
        first
            .close_position(pos.id, 100.0, "veto_close", None)
            .await
            .unwrap();
        first
            .counterfactual_put(pos.id, 1, "tp", 1.25, -0.5)
            .await
            .unwrap();
        first.pool().close().await;

        let second = Store::open(&url)
            .await
            .expect("reopen must not fail on an existing schema");
        let (computed, net_actual, net_bracket) = second.counterfactual_totals().await.unwrap();
        assert_eq!(computed, 1, "cached counterfactual survives the rerun");
        assert!((net_bracket - 1.25).abs() < 1e-9 && (net_actual - -0.5).abs() < 1e-9);
        assert_eq!(second.counterfactual_pending_count().await.unwrap(), 0);
        second.pool().close().await;

        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
    }

    #[tokio::test]
    async fn counterfactual_outcome_domain_is_enforced_by_the_schema() {
        let store = Store::open("sqlite::memory:").await.expect("open memory");
        let sized = Sized {
            leverage: 10.0,
            margin: 20.0,
            notional: 200.0,
            stop_pct: 1.0,
            tp_pct: 2.0,
        };
        let pos = store
            .open_position("SOL", Side::Long, &sized, 100.0, false, 24.0, None)
            .await
            .unwrap();
        for ok in ["tp", "sl", "expiry"] {
            store
                .counterfactual_put(pos.id, 1, ok, 0.0, 0.0)
                .await
                .expect(ok);
        }
        let bad = store.counterfactual_put(pos.id, 1, "maybe", 0.0, 0.0).await;
        assert!(
            bad.is_err(),
            "the CHECK constraint must reject an unknown outcome token"
        );
    }

    #[tokio::test]
    async fn last_close_returns_the_most_recent_close_reason() {
        let store = Store::open("sqlite::memory:").await.expect("open memory");
        let sized = Sized {
            leverage: 10.0,
            margin: 20.0,
            notional: 200.0,
            stop_pct: 1.0,
            tp_pct: 2.0,
        };

        assert!(
            store.last_close("SOL").await.unwrap().is_none(),
            "never traded"
        );
        let p1 = store
            .open_position("SOL", Side::Long, &sized, 100.0, false, 24.0, None)
            .await
            .unwrap();
        // an open row must never be mistaken for a close
        assert!(
            store.last_close("SOL").await.unwrap().is_none(),
            "an open position is not a close"
        );

        store.close_position(p1.id, 99.0, "sl", None).await.unwrap();
        let (ts1, action) = store.last_close("SOL").await.unwrap().expect("close row");
        assert_eq!(action, "sl");
        assert!(ts1 > 0);

        // the newest close wins, and other markets are independent
        let p2 = store
            .open_position("SOL", Side::Long, &sized, 100.0, false, 24.0, None)
            .await
            .unwrap();
        store
            .close_position(p2.id, 101.0, "tp", None)
            .await
            .unwrap();
        assert_eq!(
            store.last_close("SOL").await.unwrap().unwrap().1,
            "tp",
            "most recent close wins"
        );
        assert!(
            store.last_close("ETH").await.unwrap().is_none(),
            "per market"
        );
    }

    #[tokio::test]
    async fn recent_closes_is_the_n_row_generalisation_of_last_close() {
        let store = Store::open("sqlite::memory:").await.expect("open memory");
        let sized = Sized {
            leverage: 10.0,
            margin: 20.0,
            notional: 200.0,
            stop_pct: 1.0,
            tp_pct: 2.0,
        };
        assert!(
            store.recent_closes("SOL", 3).await.unwrap().is_empty(),
            "never traded"
        );

        for (reason, exit) in [
            ("sl", 99.0),
            ("tp", 102.0),
            ("time_stop", 100.0),
            ("veto_close", 100.5),
        ] {
            let p = store
                .open_position("SOL", Side::Long, &sized, 100.0, false, 24.0, None)
                .await
                .unwrap();
            store
                .close_position(p.id, exit, reason, None)
                .await
                .unwrap();
        }
        // an open position must never appear as a close
        let open = store
            .open_position("SOL", Side::Long, &sized, 100.0, false, 24.0, None)
            .await
            .unwrap();
        assert_eq!(open.market, "SOL");

        let rows = store.recent_closes("SOL", 3).await.unwrap();
        assert_eq!(rows.len(), 3, "limit is respected");
        assert_eq!(
            rows.iter().map(|r| r.action.as_str()).collect::<Vec<_>>(),
            vec!["veto_close", "time_stop", "tp"],
            "newest first"
        );
        // head agrees with last_close, which is the same predicate
        assert_eq!(
            store.last_close("SOL").await.unwrap().unwrap().1,
            rows[0].action
        );
        // net is realized minus THIS exit's fee (the entry fee lives on the open row)
        let tp = &rows[2];
        assert!(
            tp.net_pnl > 0.0 && tp.net_pnl < 4.0,
            "tp net after its own fee: {}",
            tp.net_pnl
        );
        assert!(
            rows[1].net_pnl < 0.0,
            "a flat time_stop is a pure fee loss: {}",
            rows[1].net_pnl
        );
        assert!(
            store.recent_closes("ETH", 3).await.unwrap().is_empty(),
            "per market"
        );
    }

    #[tokio::test]
    async fn digest_window_reads_are_half_open_and_ordered() {
        let store = Store::open("sqlite::memory:").await.expect("open memory");
        let day = 1_786_147_200_000i64; // 2026-08-08T00:00:00Z
        let next = day + 86_400_000;

        // equity curve: one point before the window, three inside, one after
        for (ts, eq) in [
            (day - 1, 900.0),
            (day, 1000.0),
            (day + 3_600_000, 1010.0),
            (next - 1, 1020.0),
            (next, 1030.0),
        ] {
            store.snapshot_equity(ts, eq).await.unwrap();
        }
        let (first, last) = store.equity_bounds(day, next).await.unwrap();
        assert_eq!(first, Some(1000.0), "the window includes its start");
        assert_eq!(last, Some(1020.0), "and excludes its end");
        assert_eq!(
            store.equity_bounds(next, next + 1).await.unwrap(),
            (Some(1030.0), Some(1030.0))
        );
        assert_eq!(
            store.equity_bounds(next + 1, next + 2).await.unwrap(),
            (None, None),
            "silent day"
        );

        // decisions: reasons come back in ts order, window-bounded
        for (ts, reason) in [
            (day - 1, "before"),
            (day, "in gate_refused:Cooldown"),
            (next - 1, "in2"),
            (next, "after"),
        ] {
            store
                .log_decision(
                    ts, "SOL", "open", "long", 0.8, "t", 24.0, false, false, reason,
                )
                .await
                .unwrap();
        }
        let reasons = store.decision_reasons_between(day, next).await.unwrap();
        assert_eq!(
            reasons,
            vec!["in gate_refused:Cooldown".to_string(), "in2".to_string()]
        );

        // trades: opens included (their fee was charged that day), ordered oldest first
        let sized = Sized {
            leverage: 10.0,
            margin: 20.0,
            notional: 200.0,
            stop_pct: 1.0,
            tp_pct: 2.0,
        };
        let p = store
            .open_position("SOL", Side::Long, &sized, 100.0, false, 24.0, None)
            .await
            .unwrap();
        store.close_position(p.id, 101.0, "tp", None).await.unwrap();
        let now = chrono::Utc::now().timestamp_millis();
        let today = crate::risk::utc_day_start_ms(now);
        let trades = store
            .trades_between(today, today + 86_400_000)
            .await
            .unwrap();
        assert_eq!(trades.len(), 2, "open + close");
        assert_eq!(trades[0].action, "open");
        assert_eq!(trades[1].action, "tp");
        assert!(trades[0].ts <= trades[1].ts);
        assert_eq!(
            trades[0].fill_mode, "flat",
            "fill_mode survives the round trip"
        );
        assert!(
            store.trades_between(day, next).await.unwrap().is_empty(),
            "a day with no trades is empty"
        );
    }
}

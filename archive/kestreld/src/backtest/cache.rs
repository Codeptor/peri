//! `backtest_cache.db` — the harness's candle/funding store (spec Decision 3).
//!
//! A SEPARATE sqlite file from `kestreld.db`. The daemon runs under systemd against the
//! ledger; the harness must never open, lock or migrate it, so the two databases share
//! nothing but the `Store::open` schema-init *pattern* (raw SQL, split on `;`, rerun-safe).
//!
//! Everything in here is public market data, so the cache is disposable: `CREATE TABLE IF
//! NOT EXISTS` plus `INSERT OR REPLACE` make every run idempotent, and deleting the file
//! costs one refetch.

use sqlx::SqlitePool;
use sqlx::sqlite::SqlitePoolOptions;
use thiserror::Error;

use crate::hl_rest::Candle;

/// File name of the cache, resolved against the process's cwd (spec Decision 3).
pub const CACHE_FILE: &str = "backtest_cache.db";

#[derive(Debug, Error)]
pub enum CacheError {
    #[error("sqlx error: {0}")]
    Sqlx(#[from] sqlx::Error),
}

/// Which series a coverage range belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Series {
    Candles,
    Funding,
}

impl Series {
    pub fn as_str(self) -> &'static str {
        match self {
            Series::Candles => "candles",
            Series::Funding => "funding",
        }
    }
}

/// One cached 1m candle. `Candle`'s `T/s/i/n` are dropped on the way in: the harness keys
/// rows by (market, t) and never reads them back.
#[derive(Debug, Clone, Copy, PartialEq, sqlx::FromRow)]
pub struct CachedCandle {
    pub t: i64,
    pub o: f64,
    pub h: f64,
    pub l: f64,
    pub c: f64,
    pub v: f64,
}

#[derive(Clone)]
pub struct Cache {
    pool: SqlitePool,
}

impl Cache {
    /// `sqlite:{cwd}/backtest_cache.db?mode=rwc` — the path the CLI uses.
    pub fn default_url() -> String {
        let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        format!("sqlite:{}/{}?mode=rwc", cwd.display(), CACHE_FILE)
    }

    /// Open (creating if needed) and run the idempotent schema init.
    ///
    /// ONE connection on purpose: the fetcher and the replay loop are single-threaded by
    /// design (spec Decision 6), so a pool buys nothing and a second connection would only
    /// invite `SQLITE_BUSY` on the bulk inserts. It also makes `sqlite::memory:` behave as
    /// one shared database in tests instead of a fresh empty one per connection.
    pub async fn open(url: &str) -> Result<Self, CacheError> {
        let pool = SqlitePoolOptions::new().max_connections(1).connect(url).await?;
        if !url.contains(":memory:") && !url.contains("mode=memory") {
            let _ = sqlx::query("PRAGMA journal_mode=WAL;").execute(&pool).await;
        }
        let sql = include_str!("../../migrations/backtest_0001_init.sql");
        for stmt in sql.split(';') {
            let s = stmt.trim();
            if s.is_empty() {
                continue;
            }
            sqlx::query(s).execute(&pool).await?;
        }
        Ok(Self { pool })
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// Upsert candles under `market` (the REQUESTED name, dex-prefixed — the response's `s`
    /// mirrors the request, and keying by our own name keeps `xyz:` rows addressable).
    /// Returns the number of rows written.
    pub async fn put_candles(&self, market: &str, candles: &[Candle]) -> Result<u64, CacheError> {
        if candles.is_empty() {
            return Ok(0);
        }
        let mut tx = self.pool.begin().await?;
        let mut n = 0u64;
        for c in candles {
            sqlx::query(
                "INSERT OR REPLACE INTO candles (market, t, o, h, l, c, v) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )
            .bind(market)
            .bind(c.t)
            .bind(c.o)
            .bind(c.h)
            .bind(c.l)
            .bind(c.c)
            .bind(c.v)
            .execute(&mut *tx)
            .await?;
            n += 1;
        }
        tx.commit().await?;
        Ok(n)
    }

    /// Candles for `market` in the inclusive window `[from_ms, to_ms]`, oldest first.
    pub async fn candles(&self, market: &str, from_ms: i64, to_ms: i64) -> Result<Vec<CachedCandle>, CacheError> {
        let rows = sqlx::query_as::<_, CachedCandle>(
            "SELECT t, o, h, l, c, v FROM candles WHERE market = ?1 AND t >= ?2 AND t <= ?3 ORDER BY t",
        )
        .bind(market)
        .bind(from_ms)
        .bind(to_ms)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// `(min t, max t)` of everything cached for `market`, or `None` when nothing is.
    /// This span — not per-minute presence — is what coverage means here; see
    /// [`super::fetch::missing_ranges`].
    pub async fn candle_span(&self, market: &str) -> Result<Option<(i64, i64)>, CacheError> {
        let row = sqlx::query_as::<_, (Option<i64>, Option<i64>)>(
            "SELECT MIN(t), MAX(t) FROM candles WHERE market = ?1",
        )
        .bind(market)
        .fetch_one(&self.pool)
        .await?;
        Ok(match row {
            (Some(lo), Some(hi)) => Some((lo, hi)),
            _ => None,
        })
    }

    pub async fn count_candles(&self, market: &str, from_ms: i64, to_ms: i64) -> Result<i64, CacheError> {
        let (n,) = sqlx::query_as::<_, (i64,)>(
            "SELECT COUNT(*) FROM candles WHERE market = ?1 AND t >= ?2 AND t <= ?3",
        )
        .bind(market)
        .bind(from_ms)
        .bind(to_ms)
        .fetch_one(&self.pool)
        .await?;
        Ok(n)
    }

    /// Upsert `(time, rate)` funding samples under `market`.
    pub async fn put_funding(&self, market: &str, rows: &[(i64, f64)]) -> Result<u64, CacheError> {
        if rows.is_empty() {
            return Ok(0);
        }
        let mut tx = self.pool.begin().await?;
        let mut n = 0u64;
        for (t, rate) in rows {
            sqlx::query("INSERT OR REPLACE INTO funding (market, t, rate) VALUES (?1, ?2, ?3)")
                .bind(market)
                .bind(t)
                .bind(rate)
                .execute(&mut *tx)
                .await?;
            n += 1;
        }
        tx.commit().await?;
        Ok(n)
    }

    /// Funding samples for `market` in `[from_ms, to_ms]`, oldest first — the order
    /// `FeatureEngine::seed_funding_history` requires.
    pub async fn funding(&self, market: &str, from_ms: i64, to_ms: i64) -> Result<Vec<(i64, f64)>, CacheError> {
        let rows = sqlx::query_as::<_, (i64, f64)>(
            "SELECT t, rate FROM funding WHERE market = ?1 AND t >= ?2 AND t <= ?3 ORDER BY t",
        )
        .bind(market)
        .bind(from_ms)
        .bind(to_ms)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    pub async fn funding_span(&self, market: &str) -> Result<Option<(i64, i64)>, CacheError> {
        let row = sqlx::query_as::<_, (Option<i64>, Option<i64>)>(
            "SELECT MIN(t), MAX(t) FROM funding WHERE market = ?1",
        )
        .bind(market)
        .fetch_one(&self.pool)
        .await?;
        Ok(match row {
            (Some(lo), Some(hi)) => Some((lo, hi)),
            _ => None,
        })
    }

    pub async fn count_funding(&self, market: &str, from_ms: i64, to_ms: i64) -> Result<i64, CacheError> {
        let (n,) = sqlx::query_as::<_, (i64,)>(
            "SELECT COUNT(*) FROM funding WHERE market = ?1 AND t >= ?2 AND t <= ?3",
        )
        .bind(market)
        .bind(from_ms)
        .bind(to_ms)
        .fetch_one(&self.pool)
        .await?;
        Ok(n)
    }

    /// Every market with at least one cached candle, sorted — the replay's universe when
    /// no `--markets` list is given.
    pub async fn markets(&self) -> Result<Vec<String>, CacheError> {
        let rows = sqlx::query_as::<_, (String,)>("SELECT DISTINCT market FROM candles ORDER BY market")
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.into_iter().map(|(m,)| m).collect())
    }

    /// Ranges already asked for and answered, merged and ascending.
    pub async fn coverage(&self, market: &str, series: Series) -> Result<Vec<(i64, i64)>, CacheError> {
        let rows = sqlx::query_as::<_, (i64, i64)>(
            "SELECT from_t, to_t FROM coverage WHERE market = ?1 AND series = ?2 ORDER BY from_t",
        )
        .bind(market)
        .bind(series.as_str())
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// Record `[from_ms, to_ms]` as covered, merged into what is already there. `step` is the
    /// series' granularity: ranges closer than one step are contiguous, not two islands.
    ///
    /// Merged rows are rewritten wholesale under one transaction — the row count per market
    /// stays at the number of genuinely disjoint islands (normally one).
    pub async fn add_coverage(
        &self,
        market: &str,
        series: Series,
        from_ms: i64,
        to_ms: i64,
        step: i64,
    ) -> Result<(), CacheError> {
        if to_ms < from_ms {
            return Ok(());
        }
        let mut ranges = self.coverage(market, series).await?;
        ranges.push((from_ms, to_ms));
        let merged = super::fetch::merge_ranges(ranges, step);
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM coverage WHERE market = ?1 AND series = ?2")
            .bind(market)
            .bind(series.as_str())
            .execute(&mut *tx)
            .await?;
        for (a, b) in merged {
            sqlx::query("INSERT INTO coverage (market, series, from_t, to_t) VALUES (?1, ?2, ?3, ?4)")
                .bind(market)
                .bind(series.as_str())
                .bind(a)
                .bind(b)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Flush and release the file. The CLI is a short-lived process: closing checkpoints the
    /// WAL back into the db instead of leaving the run's rows in a sidecar file.
    pub async fn close(&self) {
        self.pool.close().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candle(t: i64, o: f64, h: f64, l: f64, c: f64, v: f64) -> Candle {
        Candle {
            t,
            T: t + 59_999,
            s: "BTC".to_string(),
            i: "1m".to_string(),
            o,
            c,
            h,
            l,
            v,
            n: 1,
        }
    }

    async fn cache() -> Cache {
        Cache::open("sqlite::memory:").await.expect("open cache")
    }

    #[tokio::test]
    async fn schema_init_is_rerun_safe_on_the_same_file() {
        // Same file, opened twice — the second init must not fail and must not lose rows.
        let dir = std::env::temp_dir().join(format!("kestrel-bt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("tmp dir");
        let path = dir.join("rerun.db");
        let _ = std::fs::remove_file(&path);
        let url = format!("sqlite:{}?mode=rwc", path.display());

        let c1 = Cache::open(&url).await.expect("first open");
        c1.put_candles("BTC", &[candle(60_000, 1.0, 2.0, 0.5, 1.5, 10.0)]).await.expect("put");
        drop(c1);

        let c2 = Cache::open(&url).await.expect("second open runs the same schema init");
        assert_eq!(c2.count_candles("BTC", 0, 120_000).await.unwrap(), 1, "rows survive re-init");
        drop(c2);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(dir.join("rerun.db-wal"));
        let _ = std::fs::remove_file(dir.join("rerun.db-shm"));
    }

    #[tokio::test]
    async fn put_candles_is_idempotent_and_last_write_wins() {
        let c = cache().await;
        let batch = vec![
            candle(0, 1.0, 2.0, 0.5, 1.5, 10.0),
            candle(60_000, 1.5, 2.5, 1.0, 2.0, 20.0),
        ];
        assert_eq!(c.put_candles("IDEM", &batch).await.unwrap(), 2);
        // Same batch again: the PRIMARY KEY (market, t) collapses it — no duplicate rows.
        assert_eq!(c.put_candles("IDEM", &batch).await.unwrap(), 2);
        assert_eq!(c.count_candles("IDEM", 0, 60_000).await.unwrap(), 2, "rerun must not duplicate");

        // A corrected candle for the same minute replaces the old one.
        c.put_candles("IDEM", &[candle(0, 1.0, 9.0, 0.5, 3.0, 99.0)]).await.unwrap();
        let rows = c.candles("IDEM", 0, 60_000).await.unwrap();
        assert_eq!(rows.len(), 2);
        assert!((rows[0].c - 3.0).abs() < 1e-12, "last write wins for (market,t)");
        assert!((rows[0].v - 99.0).abs() < 1e-12);
        assert_eq!(c.candle_span("IDEM").await.unwrap(), Some((0, 60_000)));
    }

    #[tokio::test]
    async fn candles_are_window_scoped_market_scoped_and_ordered() {
        let c = cache().await;
        c.put_candles(
            "A",
            &[candle(0, 1.0, 1.0, 1.0, 1.0, 1.0), candle(120_000, 3.0, 3.0, 3.0, 3.0, 1.0), candle(60_000, 2.0, 2.0, 2.0, 2.0, 1.0)],
        )
        .await
        .unwrap();
        c.put_candles("B", &[candle(0, 9.0, 9.0, 9.0, 9.0, 1.0)]).await.unwrap();

        let rows = c.candles("A", 0, 120_000).await.unwrap();
        assert_eq!(rows.iter().map(|r| r.t).collect::<Vec<_>>(), vec![0, 60_000, 120_000], "ascending by t");
        let windowed = c.candles("A", 60_000, 60_000).await.unwrap();
        assert_eq!(windowed.len(), 1, "window is inclusive on both ends");
        assert!((windowed[0].o - 2.0).abs() < 1e-12);
        assert_eq!(c.count_candles("B", 0, 120_000).await.unwrap(), 1, "markets do not bleed into each other");
        assert_eq!(c.markets().await.unwrap(), vec!["A".to_string(), "B".to_string()]);
    }

    #[tokio::test]
    async fn coverage_merges_per_market_and_series_and_survives_reopen() {
        const M: i64 = 60_000;
        let dir = std::env::temp_dir().join(format!("kestrel-bt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("tmp dir");
        let path = dir.join("coverage.db");
        let _ = std::fs::remove_file(&path);
        let url = format!("sqlite:{}?mode=rwc", path.display());

        let c = Cache::open(&url).await.expect("open");
        assert!(c.coverage("BTC", Series::Candles).await.unwrap().is_empty());

        // Day 0, then day 1 on a later run: one merged island, not two rows.
        c.add_coverage("BTC", Series::Candles, 0, 1439 * M, M).await.unwrap();
        c.add_coverage("BTC", Series::Candles, 1440 * M, 2879 * M, M).await.unwrap();
        assert_eq!(c.coverage("BTC", Series::Candles).await.unwrap(), vec![(0, 2879 * M)]);
        // Re-recording an already covered range is a no-op.
        c.add_coverage("BTC", Series::Candles, 0, 1439 * M, M).await.unwrap();
        assert_eq!(c.coverage("BTC", Series::Candles).await.unwrap(), vec![(0, 2879 * M)]);
        // A disjoint island stays separate.
        c.add_coverage("BTC", Series::Candles, 10_000 * M, 10_100 * M, M).await.unwrap();
        assert_eq!(
            c.coverage("BTC", Series::Candles).await.unwrap(),
            vec![(0, 2879 * M), (10_000 * M, 10_100 * M)]
        );
        // Series and markets are independent keys.
        assert!(c.coverage("BTC", Series::Funding).await.unwrap().is_empty());
        assert!(c.coverage("ETH", Series::Candles).await.unwrap().is_empty());
        c.add_coverage("BTC", Series::Funding, 0, 1439 * M, M).await.unwrap();
        assert_eq!(c.coverage("BTC", Series::Funding).await.unwrap(), vec![(0, 1439 * M)]);
        c.close().await;

        // Reopening the same file sees the same coverage — this is what makes a rerun in a
        // NEW process fetch nothing.
        let c2 = Cache::open(&url).await.expect("reopen");
        assert_eq!(c2.coverage("BTC", Series::Candles).await.unwrap(), vec![(0, 2879 * M), (10_000 * M, 10_100 * M)]);
        c2.close().await;
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(dir.join("coverage.db-wal"));
        let _ = std::fs::remove_file(dir.join("coverage.db-shm"));
    }

    #[tokio::test]
    async fn funding_round_trips_and_span_is_none_when_empty() {
        let c = cache().await;
        assert_eq!(c.funding_span("BTC").await.unwrap(), None);
        assert_eq!(c.candle_span("BTC").await.unwrap(), None, "empty cache has no span");

        let rows = vec![(3_600_000i64, 0.0000125f64), (7_200_000, -0.00002)];
        assert_eq!(c.put_funding("BTC", &rows).await.unwrap(), 2);
        assert_eq!(c.put_funding("BTC", &rows).await.unwrap(), 2, "rerun is idempotent");
        assert_eq!(c.count_funding("BTC", 0, 7_200_000).await.unwrap(), 2);
        assert_eq!(c.funding_span("BTC").await.unwrap(), Some((3_600_000, 7_200_000)));
        let back = c.funding("BTC", 0, 7_200_000).await.unwrap();
        assert_eq!(back[0].0, 3_600_000);
        assert!((back[1].1 - (-0.00002)).abs() < 1e-12);
    }
}

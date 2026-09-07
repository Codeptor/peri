-- backtest_cache.db — the harness's own sqlite file, never kestreld.db (spec Decision 3).
-- Public market data only: it is a cache, so every statement is IF NOT EXISTS and every
-- write is INSERT OR REPLACE. Deleting the file costs nothing but a refetch.

CREATE TABLE IF NOT EXISTS candles (
  market TEXT NOT NULL,
  t      INTEGER NOT NULL,
  o      REAL NOT NULL,
  h      REAL NOT NULL,
  l      REAL NOT NULL,
  c      REAL NOT NULL,
  v      REAL NOT NULL,
  PRIMARY KEY (market, t)
);

CREATE TABLE IF NOT EXISTS funding (
  market TEXT NOT NULL,
  t      INTEGER NOT NULL,
  rate   REAL NOT NULL,
  PRIMARY KEY (market, t)
);

-- What was ASKED for and answered, per market and series, as merged inclusive [from_t, to_t]
-- ms ranges. Coverage cannot be inferred from the rows themselves: Hyperliquid emits no
-- candle for a minute that did not trade, and (measured 2026-08-09) serves only ~3.6 days of
-- 1m history at all, so "the venue has nothing here" is a permanent fact that has to be
-- remembered or every rerun re-probes it. Rows are rewritten merged, so this table stays a
-- handful of rows per market.
CREATE TABLE IF NOT EXISTS coverage (
  market TEXT NOT NULL,
  series TEXT NOT NULL,
  from_t INTEGER NOT NULL,
  to_t   INTEGER NOT NULL,
  PRIMARY KEY (market, series, from_t)
);

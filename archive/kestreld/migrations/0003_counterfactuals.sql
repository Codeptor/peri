-- veto counterfactual cache (Phase T2): what the bracket would have done to a position
-- the reviewer closed early. One row per closed position, computed once, kept forever —
-- the candle window it was derived from never changes, so recomputation is pure waste.
CREATE TABLE IF NOT EXISTS counterfactuals (
    position_id INTEGER PRIMARY KEY,
    computed_ts INTEGER NOT NULL,
    bracket_outcome TEXT NOT NULL CHECK (bracket_outcome IN ('tp','sl','expiry')),
    bracket_pnl REAL NOT NULL,
    actual_pnl REAL NOT NULL,
    FOREIGN KEY(position_id) REFERENCES positions(id)
);

-- Indexes for the ledger reads the gate view and analytics run on every request / screener
-- tick: entries-per-market-per-day, per-position pnl rollups, and close lookups by cause.
CREATE INDEX IF NOT EXISTS idx_positions_market_opened ON positions(market, opened_ts);
CREATE INDEX IF NOT EXISTS idx_trades_position ON trades(position_id);
CREATE INDEX IF NOT EXISTS idx_trades_action_ts ON trades(action, ts);

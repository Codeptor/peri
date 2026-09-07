ALTER TABLE positions ADD COLUMN analyst TEXT NOT NULL DEFAULT '';
ALTER TABLE decisions ADD COLUMN analyst TEXT NOT NULL DEFAULT '';
ALTER TABLE analyst_calls ADD COLUMN analyst TEXT NOT NULL DEFAULT '';
CREATE INDEX IF NOT EXISTS idx_decisions_analyst ON decisions(analyst, ts);

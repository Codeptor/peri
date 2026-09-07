CREATE TABLE IF NOT EXISTS analyst_calls (
  id INTEGER PRIMARY KEY AUTOINCREMENT, ts INTEGER NOT NULL, market TEXT NOT NULL DEFAULT '',
  trigger TEXT NOT NULL, prompt TEXT NOT NULL, response_raw TEXT NOT NULL DEFAULT '',
  outcome_kind TEXT NOT NULL DEFAULT 'ok', parsed_json TEXT, latency_ms INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS chat_messages (
  id INTEGER PRIMARY KEY AUTOINCREMENT, ts INTEGER NOT NULL,
  role TEXT NOT NULL, text TEXT NOT NULL, action_json TEXT
);
CREATE INDEX IF NOT EXISTS idx_analyst_calls_ts ON analyst_calls(ts DESC);
CREATE INDEX IF NOT EXISTS idx_chat_messages_ts ON chat_messages(ts DESC);

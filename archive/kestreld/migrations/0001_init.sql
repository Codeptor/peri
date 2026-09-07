-- kestreld v1 persistence — WAL, paper ledger, news, meta
-- Plan A Task 5 exact tables

CREATE TABLE IF NOT EXISTS positions (
    id INTEGER PRIMARY KEY,
    market TEXT NOT NULL,
    side TEXT NOT NULL,
    entry_px REAL NOT NULL,
    size REAL NOT NULL,
    leverage REAL NOT NULL,
    margin REAL NOT NULL,
    sl_px REAL NOT NULL,
    tp_px REAL NOT NULL,
    opened_ts INTEGER NOT NULL,
    status TEXT NOT NULL DEFAULT 'open',
    closed_ts INTEGER,
    horizon_hours REAL
);

CREATE TABLE IF NOT EXISTS trades (
    id INTEGER PRIMARY KEY,
    position_id INTEGER NOT NULL,
    market TEXT NOT NULL,
    action TEXT NOT NULL,
    px REAL NOT NULL,
    size REAL NOT NULL,
    fee REAL NOT NULL,
    realized_pnl REAL NOT NULL,
    ts INTEGER NOT NULL,
    FOREIGN KEY(position_id) REFERENCES positions(id)
);

CREATE TABLE IF NOT EXISTS equity (
    ts INTEGER PRIMARY KEY,
    equity REAL NOT NULL
);

CREATE TABLE IF NOT EXISTS decisions (
    id INTEGER PRIMARY KEY,
    ts INTEGER NOT NULL,
    market TEXT NOT NULL,
    action TEXT NOT NULL,
    side TEXT,
    conviction REAL NOT NULL,
    thesis TEXT NOT NULL,
    horizon_hours REAL,
    vetoed INTEGER NOT NULL,
    executed INTEGER NOT NULL,
    reason TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS news (
    id INTEGER PRIMARY KEY,
    ts INTEGER NOT NULL,
    source TEXT NOT NULL,
    title TEXT NOT NULL,
    body TEXT NOT NULL,
    url TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS news_markets (
    news_id INTEGER NOT NULL,
    market TEXT NOT NULL,
    PRIMARY KEY (news_id, market),
    FOREIGN KEY(news_id) REFERENCES news(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS news_seen (
    hash TEXT PRIMARY KEY,
    ts INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS meta (
    k TEXT PRIMARY KEY,
    v TEXT NOT NULL
);

-- fill_mode attribution for Batch-1 book-vs-flat fills
ALTER TABLE trades ADD COLUMN fill_mode TEXT NOT NULL DEFAULT 'flat';

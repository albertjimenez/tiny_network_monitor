-- Single source of truth for the SQLite schema.
-- Loaded at runtime via `include_str!` in `src/db.rs` (Store::init) and
-- executed against `:memory:` by `build.rs`, so an invalid schema fails
-- the build instead of first boot.
PRAGMA journal_mode=WAL;
PRAGMA synchronous=NORMAL;
CREATE TABLE IF NOT EXISTS checks (
   id INTEGER PRIMARY KEY AUTOINCREMENT,
   ts INTEGER NOT NULL,
   target TEXT NOT NULL,
   success INTEGER NOT NULL,
   latency_ms INTEGER,
   error TEXT
);
CREATE INDEX IF NOT EXISTS idx_checks_ts ON checks(ts);
CREATE INDEX IF NOT EXISTS idx_checks_target_ts ON checks(target, ts);

// Opens the SQLite file and creates the tables if they don't exist.
// Go later: internal/store/db.go (modernc.org/sqlite, database/sql)
import type { DatabaseSync } from "node:sqlite";

// Path on the laptop: data/pios.db (git-ignored). Create the data/ folder if missing (mkdirSync, recursive).
// Tools: new DatabaseSync(path) from "node:sqlite" · db.exec(sql) runs SQL with no result.
//
// Run once after opening:
//   PRAGMA journal_mode = WAL;   ← readers (the API) don't block the writer (the loop)
//
// Table for the 24 h trend (one row = one metric at one minute):
//   CREATE TABLE IF NOT EXISTS trend (
//     ts     INTEGER NOT NULL,   -- ms since epoch
//     metric TEXT    NOT NULL,   -- "cpu.tempC", "cpu.percent", …
//     value  REAL    NOT NULL
//   );
//   CREATE INDEX IF NOT EXISTS trend_metric_ts ON trend (metric, ts);
//     ↑ makes "all cpu.tempC rows of the last 24 h" fast, and the prune too.
// (Phase 2 adds the incidents table here.)
export function openDb(path: string): DatabaseSync {
  // TODO
  throw new Error("not implemented");
}

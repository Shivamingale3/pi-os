// Rolling 24 h trend: once a minute, store the latest reading of a few metrics; delete rows > 24 h.
// Go later: internal/store/trend.go
import type { DatabaseSync } from "node:sqlite";
import type { Snapshot } from "../types.ts";

export const TREND_WINDOW_MS = 24 * 60 * 60 * 1000;

// Which metrics go in the trend, and how to read each from a Snapshot.
// null (e.g. no temp sensor) → skip that row, don't store 0.
// Edit freely: adding a metric needs no schema change.
export const TREND_METRICS: Record<string, (s: Snapshot) => number | null> = {
  // TODO, e.g.:
  // "cpu.percent": (s) => s.cpu.percent,
  // "cpu.tempC":   (s) => s.cpu.tempC,
  // "mem.usedPercent", "disk.usedPercent", "load.1m", "net.rxBytesPerSec" (sum of interfaces), …
};

// Inserts one row per metric for this snapshot, inside ONE transaction.
// Tools: db.prepare("INSERT INTO trend (ts, metric, value) VALUES (?, ?, ?)") → stmt.run(ts, metric, value)
//        db.exec("BEGIN") … db.exec("COMMIT"); on error db.exec("ROLLBACK") and rethrow.
// Prepared statement + "?" placeholders = never build SQL by gluing strings (SQL injection habit).
export function writeTrend(db: DatabaseSync, s: Snapshot): void {
  // TODO
}

// Deletes rows older than the window. Returns how many were deleted (stmt.run(...).changes).
export function pruneTrend(db: DatabaseSync, now: number): number {
  // TODO: DELETE FROM trend WHERE ts < ?
  throw new Error("not implemented");
}

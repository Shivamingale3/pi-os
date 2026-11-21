// The latest Snapshot, in memory only. Never written to disk.
// Phase 3's GET /now just returns getLatest().
// Go later: a struct with a sync.RWMutex (the loop writes, the API reads, from different goroutines).
//   In Node there's one thread, so a plain variable is enough.
import type { Snapshot } from "./types.ts";

// TODO: one module-level variable, starts as null (nothing collected yet).

export function setLatest(s: Snapshot): void {
  // TODO
}

export function getLatest(): Snapshot | null {
  // TODO
  return null;
}

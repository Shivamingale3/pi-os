// Collects one full Snapshot. Used by the loop every 5 s.
// Go later: internal/collect/snapshot.go
import type { Snapshot } from "./types.ts";

// TODO: move the body of main() from main.ts here, and add collectNetwork().
//   cpu, network and docker are async and independent → one Promise.all for the three
//   (cpu and network both wait 500 ms; in parallel the whole snapshot still takes ~0.5 s).
export async function collectSnapshot(): Promise<Snapshot> {
  // TODO
  throw new Error("not implemented");
}

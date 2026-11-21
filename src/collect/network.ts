// rx/tx bytes per second, per real interface
// Go later: internal/collect/network.go
import { readFileSync } from "node:fs";
import type { Network } from "../types.ts";
import { parseNum } from "../parse.ts";

// Source: /proc/net/dev. 2 header lines, then one line per interface:
//   "wlp1s0: 686784502  592971 0 0 0 0 0 0 105252221  222908 0 2 0 0 0 0"
//    name  : [0] rx bytes  …                [8] tx bytes  …
//   Counters since boot (like /proc/stat), so: read, wait 500 ms, read, Δbytes ÷ Δseconds.
//   Measure Δseconds yourself (performance.now() before/after), don't assume exactly 0.5.
//
// Keep only real interfaces (allowlist): names starting with eth, en, wl, tailscale.
//   Everything else (lo, docker0, veth* = one per container, br-*) is dropped.
// Watch out: the name can be glued to the first number ("eth0:123") on busy machines,
//   so split each line on ":" first, then split the right side on /\s+/.
// Interface that appears/disappears between the two reads (VPN up/down) → just skip it.

type Counters = Map<string, { rx: number; tx: number }>; // name → bytes since boot
const allowList = ["eth", "en", "wl", "tailscale"];

function readCounters(): Counters {
  const networkFile = readFileSync("/proc/net/dev", "utf-8").trim();
  const networkLines = networkFile.split(/\s+/).slice(2);
  const counters = new Map<string, { rx: number; tx: number }>();

  for (const line of networkLines) {
    if (!line) continue;
    const [label, ...valuesStr] = line.split(":");
    if (!label) continue;
    for (const prefix of allowList) {
      if (!label.startsWith(prefix)) {
        continue;
      }
    }
    const metrics = valuesStr.map((value, idx) =>
      parseNum(value, `${label} netdev metric ${idx}`),
    );

    counters.set(label, { rx: metrics[0] ?? 0, tx: metrics[8] ?? 0 });
  }
  return counters;
}

export async function collectNetwork(): Promise<Network> {
  throw new Error("not implemented");
}

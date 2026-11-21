import { readFileSync } from "node:fs";
import type { Host } from "../types.ts";
import { parseNum } from "../parse.ts";

export function collectHost(): Host {
  const hostnameRaw = readFileSync("/proc/sys/kernel/hostname", "utf-8");
  const hostname = hostnameRaw.trim();

  if (!hostname) throw new Error("bad hostname");

  const uptimeRaw = readFileSync("/proc/uptime", "utf-8");
  const [uptime] = uptimeRaw.split(" ");
  if (!uptime) throw new Error("bad uptime");

  const loadavgRaw = readFileSync("/proc/loadavg", "utf-8");
  const [load1, load5, load15] = loadavgRaw.split(" ");
  if (!load1) throw new Error("bad load1");
  if (!load5) throw new Error("bad load5");
  if (!load15) throw new Error("bad load15");

  return {
    hostname,
    load: [
      parseNum(load1, "loadavg 1m"),
      parseNum(load5, "loadavg 5m"),
      parseNum(load15, "loadavg 15m"),
    ],
    uptimeSec: parseNum(uptime, "uptimeSec"),
  };
}

import { readFileSync } from "node:fs";
import { parseNum } from "../parse.ts";
import type { Memory } from "../types.ts";

export function collectMemory(): Memory {
  const rawMemInfo = readFileSync("/proc/meminfo", "utf-8");
  const meminfoLines = rawMemInfo.trim().split("\n");
  const meminfo = new Map<string, number>();
  for (const line of meminfoLines) {
    const [key, value] = line.split(":");
    if (!key || !value) throw new Error("Invalid memory format");
    const [num] = value.trim().split(/\s+/);
    if (!num) throw new Error("Invalid memory format");
    meminfo.set(key.trim(), parseNum(num, key.trim()));
  }

  const memTotal = meminfo.get("MemTotal");
  if (!memTotal) {
    throw new Error("Invalid value for total memory!");
  }

  const memAvailable = meminfo.get("MemAvailable");
  if (memAvailable === undefined) {
    throw new Error("Invalid value for available memory!");
  }

  const totalBytes = memTotal * 1024;
  const usedBytes = totalBytes - memAvailable * 1024;
  const availableBytes = memAvailable * 1024;

  return {
    totalBytes,
    usedBytes,
    availableBytes,
  };
}

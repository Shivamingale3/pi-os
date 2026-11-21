import { readFileSync } from "node:fs";
import { parseNum } from "../parse.ts";
import type { Cpu } from "../types.ts";

function readTicks(): { idle: number; total: number } {
  const firstLine = readFileSync("/proc/stat", "utf-8").split("\n")[0];
  if (!firstLine) {
    throw new Error("Invalid /proc/stat content.");
  }
  const [user, nice, system, idle, iowait, irq, softirq, steal] = firstLine
    .trim()
    .split(/\s+/)
    .slice(1)
    .map((x, i) => parseNum(x, `/proc/stat field ${i}`));
  if (
    user === undefined ||
    nice === undefined ||
    system === undefined ||
    idle === undefined ||
    iowait === undefined ||
    irq === undefined ||
    softirq === undefined ||
    steal === undefined
  ) {
    throw new Error("Invalid tick values");
  }

  return {
    idle: idle + iowait,
    total: user + nice + system + idle + iowait + irq + softirq + steal,
  };
}

export async function collectCpu(): Promise<Cpu> {
  const previous = readTicks();
  await new Promise((r) => setTimeout(r, 500));
  const current = readTicks();
  const idle = current.idle - previous.idle;
  const total = current.total - previous.total;
  const percent = total === 0 ? 0 : 100 * (1 - idle / total);

  let temp: number | null = null;
  try {
    const rawTempC = readFileSync(
      "/sys/class/thermal/thermal_zone0/temp",
      "utf-8",
    ).trim();
    temp = parseNum(rawTempC, "tempC") / 1000;
  } catch {
    temp = null;
  }
  return {
    percent,
    tempC: temp,
  };
}

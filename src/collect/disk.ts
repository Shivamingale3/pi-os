import { statfsSync } from "node:fs";
import type { Disk } from "../types.ts";

export function collectDisk(): Disk {
  const disk = statfsSync("/");
  const used =
    (100 * (disk.blocks - disk.bfree)) /
    (disk.blocks - disk.bfree + disk.bavail);
  return {
    usedPercent: used,
  };
}

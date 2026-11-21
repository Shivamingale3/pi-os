import { collectCpu } from "./collect/cpu.ts";
import { collectDisk } from "./collect/disk.ts";
import { collectDocker } from "./collect/docker.ts";
import { collectHost } from "./collect/host.ts";
import { collectMemory } from "./collect/memory.ts";
import type { Snapshot } from "./types.ts";
async function main(): Promise<void> {
  const [cpu, docker] = await Promise.all([collectCpu(), collectDocker()]);
  const snapshot: Snapshot = {
    ts: Date.now(),
    cpu,
    disk: collectDisk(),
    docker,
    host: collectHost(),
    memory: collectMemory(),
  };
  console.log(JSON.stringify(snapshot, null, 2));
}

await main();

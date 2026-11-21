// The shape of one snapshot. Every collector fills one part.
// Rule: if a source is missing (e.g. no thermal sensor, no Docker), use `null`, never crash.
// Go later: each type becomes a struct, `number | null` becomes a pointer (*float64) or a value + ok flag.

export type Host = {
  hostname: string;
  uptimeSec: number; // seconds since boot
  load: [number, number, number]; // 1, 5, 15 min
};

export type Cpu = {
  percent: number; // 0–100, busy share between two reads
  tempC: number | null; // null if no sensor
};

export type Memory = {
  usedBytes: number;
  totalBytes: number;
  availableBytes: number;
};

export type Disk = {
  usedPercent: number; // of "/"
};

export type Docker = {
  running: number;
  total: number;
};

// One network interface, rates measured between two reads (like CPU %).
export type NetIface = {
  name: string;          // "eth0", "wlp1s0", "tailscale0"
  rxBytesPerSec: number; // received
  txBytesPerSec: number; // sent
};

export type Network = {
  interfaces: NetIface[]; // real interfaces only: eth*, en*, wl*, tailscale* (allowlist)
};

export type Snapshot = {
  ts: number; // when it was taken, ms since epoch (Date.now())
  host: Host;
  cpu: Cpu;
  memory: Memory;
  disk: Disk;
  docker: Docker | null; // null if Docker isn't reachable
  network: Network;
};

export type DockerSocketResponseItem = {
  Id: string;
  Names: string[];
  Image: string;
  ImageID: string;
  ImageManifestDescriptor: {
    mediaType: string;
    digest: string;
    size: number;
    platform: object[];
  };
  Command: string;
  Created: number;
  Ports: object[][];
  Labels: Record<string, string>;
  State: string;
  Status: string;
  HostConfig: { NetworkMode: string };
  Health: { Status: string; FailingStreak: number };
  NetworkSettings: { Networks: object[] };
  Mounts: object[][];
};

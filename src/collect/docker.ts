import { request } from "node:http";
import type { Docker, DockerSocketResponseItem } from "../types.ts";

export async function collectDocker(): Promise<Docker | null> {
  try {
    const chunks: Buffer[] = [];
    const response = new Promise((resolve, reject) => {
      const req = request(
        {
          socketPath: "/var/run/docker.sock",
          path: "/containers/json?all=true",
          method: "GET",
          timeout: 2000,
        },
        (res) => {
          if (res.statusCode !== 200) {
            res.resume();
            reject(new Error("Docker API returned non-200 status"));
            return;
          }
          res.on("data", (chunk) => chunks.push(chunk));
          res.on("end", () => {
            try {
              resolve(JSON.parse(Buffer.concat(chunks).toString()));
            } catch (e) {
              reject(e);
            }
          });
        },
      )
        .on("error", reject)
        .on("timeout", () => {
          req.destroy(new Error("docker timeout"));
        })
        .end();
    });
    const containers = await response;
    if (!Array.isArray(containers))
      throw new Error("Docker API response is not an array");
    const runningContainers = containers.filter(
      (container: DockerSocketResponseItem) => container.State === "running",
    );
    return {
      running: runningContainers.length,
      total: containers.length,
    };
  } catch {
    return null;
  }
}

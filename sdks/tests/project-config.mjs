import assert from "node:assert/strict";
import { mkdtemp, mkdir, writeFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { resolve, join } from "node:path";
import { LocalServer } from "../typescript/dist/index.js";

const root = await mkdtemp(join(tmpdir(), "haarness-sdk-project-config-"));
const workspace = join(root, "project");
const dataDir = join(root, "state");
await mkdir(join(workspace, ".haarness/commands"), { recursive: true });
await mkdir(join(dataDir, "config/commands"), { recursive: true });
await writeFile(join(workspace, ".haarness/commands/project-proof.md"), "Project command");
await writeFile(join(dataDir, "config/commands/global-proof.md"), "Global command");
try {
  for (const [label, options, enabled] of [
    ["default", {}, true],
    ["explicit opt-out", { disableProjectConfig: true }, false],
    ["legacy false", { trustProject: false }, false],
    ["legacy true", { trustProject: true }, true],
    ["environment opt-out", { env: { HAARNESS_DISABLE_PROJECT_CONFIG: "1" } }, false],
  ]) {
    const server = await LocalServer.start({
      executable: resolve(process.env.HAARNESS_BIN ?? "target/debug/haarness"),
      workspace, dataDir, ...options,
      env: { HOME: root, HAARNESS_HOME: root, HAARNESS_TEST_PROVIDER: "1",
        HAARNESS_DISABLE_PROJECT_CONFIG: "0", ...options.env },
    });
    try {
      assert.equal((await server.client.info()).projectTrusted, enabled, label);
      const commands = (await server.client.commands()).map(item => item.name);
      assert.equal(commands.includes("project-proof"), enabled, label);
      assert.ok(commands.includes("global-proof"));
      console.log("PASS TypeScript SDK", label);
    } finally { await server.close(); }
  }
} finally { await rm(root, { recursive: true, force: true }); }

import test from "node:test";
import assert from "node:assert/strict";
import { chmod, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { LocalServer } from "../dist/index.js";

test("a local server that cannot start says why", async () => {
  const folder = await mkdtemp(path.join(tmpdir(), "haarness-sdk-start-"));
  try {
    const executable = path.join(folder, "haarness");
    await writeFile(executable, "#!/bin/sh\necho 'warming up' >&2\necho 'Error: a Haarness server already runs in this folder' >&2\nexit 1\n");
    await chmod(executable, 0o755);
    await assert.rejects(
      LocalServer.start({ executable, workspace: folder, startupTimeoutMs: 5000 }),
      { message: "Haarness failed to start: Error: a Haarness server already runs in this folder" },
    );
  } finally {
    await rm(folder, { recursive: true, force: true });
  }
});

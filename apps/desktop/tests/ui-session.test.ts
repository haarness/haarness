import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, mkdir, writeFile, readFile, rm } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { createUiSession } from "../src/main/ui-session";

test("running UI keeps its entry point, deferred modules and preload across build replacement", async () => {
  const output = await mkdtemp(join(tmpdir(), "haarness-ui-build-"));
  let session: Awaited<ReturnType<typeof createUiSession>> | undefined;
  try {
    await mkdir(join(output, "renderer/assets"), { recursive: true });
    await mkdir(join(output, "preload"));
    await writeFile(join(output, "renderer/index.html"), "old entry point");
    await writeFile(
      join(output, "renderer/assets/Settings-old.js"),
      "old deferred module",
    );
    await writeFile(join(output, "preload/index.cjs"), "old bridge");
    session = await createUiSession(output);
    await rm(output, { recursive: true });
    assert.equal(await readFile(session.renderer, "utf8"), "old entry point");
    assert.equal(
      await readFile(
        join(session.renderer, "../assets/Settings-old.js"),
        "utf8",
      ),
      "old deferred module",
    );
    assert.equal(await readFile(session.preload, "utf8"), "old bridge");
    session.dispose();
    await assert.rejects(readFile(session.renderer));
  } finally {
    session?.dispose();
    await rm(output, { recursive: true, force: true });
  }
});

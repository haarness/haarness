import { cp, mkdtemp, rm } from "node:fs/promises";
import { rmSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";

/** Keep an unpackaged app's loaded entry point and deferred imports together.
 * Rebuilding out/ must not remove code still needed by a running window. */
export async function createUiSession(output: string) {
  const directory = await mkdtemp(join(tmpdir(), "haarness-ui-"));
  try {
    await cp(join(output, "renderer"), join(directory, "renderer"), {
      recursive: true,
    });
    await cp(join(output, "preload"), join(directory, "preload"), {
      recursive: true,
    });
    return {
      renderer: join(directory, "renderer/index.html"),
      preload: join(directory, "preload/index.cjs"),
      dispose: () => rmSync(directory, { recursive: true, force: true }),
    };
  } catch (error) {
    await rm(directory, { recursive: true, force: true });
    throw error;
  }
}

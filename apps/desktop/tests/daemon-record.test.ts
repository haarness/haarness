import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, mkdir, realpath, rm, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { cliCacheFolder, daemonRunning } from "../src/main/daemon-record";

test("the CLI's cache folder is found as haarness finds it", () => {
  assert.equal(cliCacheFolder({ HAARNESS_DATA_DIR: "/d" }, "/home/me"), "/d/cache");
  assert.equal(cliCacheFolder({ HAARNESS_HOME: "/h" }, "/home/me"), "/h/cache");
  // HAARNESS_HOME must be absolute, or haarness refuses to start at all.
  assert.equal(cliCacheFolder({ HAARNESS_HOME: "h" }, "/home/me"), undefined);
  assert.equal(cliCacheFolder({ XDG_CACHE_HOME: "/x" }, "/home/me"), "/x/haarness");
  assert.equal(cliCacheFolder({ XDG_CACHE_HOME: "x" }, "/home/me"), "/home/me/.cache/haarness");
  assert.equal(cliCacheFolder({}, "/home/me"), "/home/me/.cache/haarness");
  // A relative data folder is not one haarness would use.
  assert.equal(cliCacheFolder({ HAARNESS_DATA_DIR: "d" }, "/home/me"), "/home/me/.cache/haarness");
});

test("a folder's daemon counts as running only when its record's process lives and its server answers", async () => {
  const root = await realpath(await mkdtemp(join(tmpdir(), "haarness-daemon-record-")));
  try {
    const home = join(root, "Chat folders");
    const other = join(root, "project");
    await mkdir(home);
    await mkdir(other);
    // The record may name the folder through a link.
    await symlink(home, join(root, "linked-home"));
    const env = { HAARNESS_DATA_DIR: join(root, "data") };
    const records = join(root, "data", "cache", "workspaces");
    const record = async (id: string, value: unknown) => {
      await mkdir(join(records, id), { recursive: true });
      await writeFile(join(records, id, "runtime.json"), typeof value === "string" ? value : JSON.stringify(value));
    };
    const asked: string[] = [];
    const running = (alive: number[], answering: string[]) =>
      daemonRunning(home, {
        env,
        home: root,
        alive: (pid) => alive.includes(pid),
        probe: async (origin, token) => {
          asked.push(origin + " " + token);
          return answering.includes(token);
        },
      });

    // No records at all.
    assert.equal(await running([1], ["a"]), false);
    await record("garbage", "{");
    await record("other", { url: "http://127.0.0.1:4000", token: "o", pid: 2, workspace: other });
    assert.equal(await running([1, 2], ["o"]), false);
    // Another computer's address is never sent the token.
    await record("far", { url: "http://192.0.2.1:4001", token: "f", pid: 3, workspace: home });
    assert.equal(await running([3], ["f"]), false);
    assert.deepEqual(asked, []);
    await record("home", { url: "http://0.0.0.0:4002", token: "a", pid: 1, workspace: join(root, "linked-home") });
    // A record left by a server that crashed.
    assert.equal(await running([], ["a"]), false);
    assert.deepEqual(asked, []);
    // Alive, but its port is someone else's now.
    assert.equal(await running([1], []), false);
    assert.deepEqual(asked, ["http://127.0.0.1:4002 a"]);
    assert.equal(await running([1], ["a"]), true);
    // A chat home not made yet has no daemon.
    assert.equal(
      await daemonRunning(join(root, "missing"), { env, home: root, alive: () => true, probe: async () => true }),
      false,
    );
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

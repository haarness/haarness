import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, mkdir, realpath, rm, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { cliCacheFolder, daemonRunning, servedFolder } from "../src/main/daemon-record";

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

test("a server counts as the record's only on the terms of the Chrome bridge's judge()", () => {
  const record = { pid: 7, workspace: "/home/Chat folders" };
  assert.equal(servedFolder(record, { name: "haarness", pid: 7, workspace: "/home/Chat folders" }), "/home/Chat folders");
  // Older servers may leave the pid or the folder out.
  assert.equal(servedFolder(record, { name: "haarness" }), "/home/Chat folders");
  assert.equal(servedFolder({ pid: 7 }, { name: "haarness", workspace: "/w" }), "/w");
  assert.equal(servedFolder({ pid: 7 }, { name: "haarness" }), undefined);
  // Something else answers on the port, or another process, or another folder.
  assert.equal(servedFolder(record, { name: "other", pid: 7, workspace: "/home/Chat folders" }), undefined);
  assert.equal(servedFolder(record, { pid: 7, workspace: "/home/Chat folders" }), undefined);
  assert.equal(servedFolder(record, { name: "haarness", pid: 8 }), undefined);
  assert.equal(servedFolder(record, { name: "haarness", workspace: "/home/project" }), undefined);
});

test("a folder's daemon counts as running only when its record names that folder, its process lives and the server there is that one", async () => {
  const root = await realpath(await mkdtemp(join(tmpdir(), "haarness-daemon-record-")));
  try {
    const home = join(root, "Chat folders");
    const other = join(root, "project");
    await mkdir(home);
    await mkdir(other);
    await symlink(home, join(root, "linked-home"));
    const env = { HAARNESS_DATA_DIR: join(root, "data") };
    const records = join(root, "data", "cache", "workspaces");
    const record = async (id: string, value: unknown) => {
      await mkdir(join(records, id), { recursive: true });
      await writeFile(join(records, id, "runtime.json"), typeof value === "string" ? value : JSON.stringify(value));
    };
    const asked: string[] = [];
    const running = (alive: number[], answers: Record<string, object>, folder = home) =>
      daemonRunning(folder, {
        env,
        home: root,
        alive: (pid) => alive.includes(pid),
        probe: async (origin, token) => {
          asked.push(origin + " " + token);
          return answers[token];
        },
      });
    const haarness = (pid: number, workspace = home) => ({ name: "haarness", pid, workspace });

    // No records at all.
    assert.equal(await running([1], { a: haarness(1) }), false);
    await record("garbage", "{");
    await record("other", { url: "http://127.0.0.1:4000", token: "o", pid: 2, workspace: other });
    assert.equal(await running([1, 2], { o: haarness(2, other) }), false);
    // A record names its folder as the runtime wrote it, canonical: one
    // naming it through a link is another folder's, and never asked.
    await record("linked", { url: "http://127.0.0.1:4003", token: "l", pid: 4, workspace: join(root, "linked-home") });
    assert.equal(await running([4], { l: haarness(4) }), false);
    // Another computer's address is never sent the token.
    await record("far", { url: "http://192.0.2.1:4001", token: "f", pid: 3, workspace: home });
    assert.equal(await running([3], { f: haarness(3) }), false);
    assert.deepEqual(asked, []);
    await record("home", { url: "http://0.0.0.0:4002", token: "a", pid: 1, workspace: home });
    // A record left by a server that crashed.
    assert.equal(await running([], { a: haarness(1) }), false);
    assert.deepEqual(asked, []);
    // Alive, but its port answers nothing, or answers as something else.
    assert.equal(await running([1], {}), false);
    assert.deepEqual(asked, ["http://127.0.0.1:4002 a"]);
    assert.equal(await running([1], { a: haarness(9) }), false);
    assert.equal(await running([1], { a: haarness(1, other) }), false);
    assert.equal(await running([1], { a: { ...haarness(1), name: "proxy" } }), false);
    assert.equal(await running([1], { a: haarness(1) }), true);
    // The chat home asked for by a link is resolved once, to the same folder.
    assert.equal(await running([1], { a: haarness(1) }, join(root, "linked-home")), true);
    // A chat home not made yet has no daemon.
    assert.equal(await running([1], { a: haarness(1) }, join(root, "missing")), false);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test("the server's word is read without following a redirect, which counts as no answer", async () => {
  const { createServer } = await import("node:http");
  const root = await realpath(await mkdtemp(join(tmpdir(), "haarness-daemon-redirect-")));
  const seen: string[] = [];
  const server = createServer((request, response) => {
    seen.push(request.url + " " + (request.headers.authorization ?? ""));
    if (request.url === "/v1/info" && request.headers.authorization === "Bearer moved") {
      response.writeHead(307, { location: "/elsewhere" });
      response.end();
    } else if (request.url === "/v1/info" && request.headers.authorization === "Bearer good") {
      response.writeHead(200, { "content-type": "application/json" });
      response.end(JSON.stringify({ name: "haarness", pid: process.pid, workspace: home }));
    } else {
      // Where the redirect points: never asked.
      response.writeHead(200, { "content-type": "application/json" });
      response.end(JSON.stringify({ name: "haarness", pid: process.pid, workspace: home }));
    }
  });
  const home = join(root, "Chat folders");
  await mkdir(home);
  await new Promise<void>((done) => server.listen(0, "127.0.0.1", done));
  try {
    const port = (server.address() as { port: number }).port;
    const env = { HAARNESS_DATA_DIR: join(root, "data") };
    const write = async (token: string) => {
      await mkdir(join(root, "data", "cache", "workspaces", "home"), { recursive: true });
      await writeFile(
        join(root, "data", "cache", "workspaces", "home", "runtime.json"),
        JSON.stringify({ url: `http://127.0.0.1:${port}`, token, pid: process.pid, workspace: home }),
      );
    };
    await write("moved");
    assert.equal(await daemonRunning(home, { env, home: root }), false);
    assert.deepEqual(seen, ["/v1/info Bearer moved"]);
    await write("good");
    assert.equal(await daemonRunning(home, { env, home: root }), true);
  } finally {
    server.close();
    await rm(root, { recursive: true, force: true });
  }
});

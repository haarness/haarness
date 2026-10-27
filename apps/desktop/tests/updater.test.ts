import test from "node:test";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdtemp, mkdir, readFile, readdir, rm, stat, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { CHECK_EVERY_MS, FIRST_CHECK_MS, RETRY_AFTER_MS, UpdateError, createUpdater, updateError, type Install, type UpdaterOptions } from "../src/main/updater";
import type { UpdateState } from "../src/updates";

const SERVICE = "https://haarness.example";
/** The updater's own schedule: first check, every half hour, retries, a wake. */
const schedule = new Set([FIRST_CHECK_MS, CHECK_EVERY_MS, ...RETRY_AFTER_MS, 5_000]);
const EDGE = "https://dl.haarness.example/releases/a1/Haarness-9.9.9-mac-arm64.zip";
const bytes = Buffer.from("the new Haarness ".repeat(4096));
const sha = (value: Buffer) => createHash("sha256").update(value).digest("hex");

interface Served {
  version?: string;
  status?: number;
  body?: unknown;
  /** Bytes the edge sends, and when it breaks off. */
  edge?: (request: { range?: string; ifRange?: string }) => Response;
  service?: (request: { range?: string; ifRange?: string }) => Response;
}

function manifest(version: string, file = bytes) {
  return {
    channel: "stable",
    manifest: {
      version,
      products: {
        desktop: {
          version,
          notes_url: "https://haarness.example/notes",
          artifacts: [
            { id: "a1", os: "macos", arch: "arm64", kind: "zip", file: `Haarness-${version}-mac-arm64.zip`, url: EDGE, size: file.length, digest: { sha256: sha(file) } },
            { id: "a2", os: "macos", arch: "x64", kind: "zip", file: `Haarness-${version}-mac-x64.zip`, url: EDGE, size: 9, digest: { sha256: "0".repeat(64) } },
          ],
        },
      },
    },
  };
}

/** A body that sends `chunks`, then ends, or breaks off when `fail` is set. */
function stream(chunks: Buffer[], fail?: Error) {
  let index = 0;
  return new ReadableStream<Uint8Array>({
    pull(controller) {
      if (index < chunks.length) controller.enqueue(new Uint8Array(chunks[index++]));
      else if (fail) controller.error(fail);
      else controller.close();
    },
  });
}
const whole = (headers: Record<string, string> = { etag: '"v1"' }) =>
  new Response(stream([bytes.subarray(0, 20_000), bytes.subarray(20_000)]), { status: 200, headers });

async function fixture(serve: Served, extra: Partial<UpdaterOptions> & { install?: Install; version?: string } = {}) {
  const dir = await mkdtemp(join(tmpdir(), "haarness-updater-"));
  const states: UpdateState[] = [];
  const requests: { url: string; range?: string; ifRange?: string }[] = [];
  const timers: { ms: number; callback: () => void }[] = [];
  const launched: { staged: string; version: string; relaunch: boolean }[] = [];
  const staged: string[] = [];
  let quits = 0;
  let automatic = true;
  const options: UpdaterOptions = {
    version: extra.version ?? "0.1.5",
    os: "macos",
    arch: "arm64",
    install: extra.install ?? { method: "restart", kind: "zip", target: "/Applications/Haarness.app" },
    releaseUrl: SERVICE,
    dir,
    async fetch(url, init) {
      const headers = (init.headers ?? {}) as Record<string, string>;
      const request = { url, range: headers.range, ifRange: headers["if-range"] };
      requests.push(request);
      if (url.endsWith("/api/v1/releases/stable/desktop")) {
        if (serve.status && serve.status !== 200) return new Response("no", { status: serve.status });
        return Response.json(serve.body ?? manifest(serve.version ?? "9.9.9"));
      }
      if (url === EDGE) return serve.edge ? serve.edge(request) : whole();
      if (url === `${SERVICE}/api/v1/releases/artifacts/a1`) return serve.service ? serve.service(request) : whole();
      throw new TypeError("fetch failed");
    },
    automatic: () => automatic,
    emit: (state) => states.push(state),
    log: () => {},
    async stage({ file, folder, version }) {
      assert.equal(sha(await readFile(file)), sha(bytes));
      const path = join(folder, "Haarness.app");
      await mkdir(path, { recursive: true });
      await writeFile(join(path, "version"), version);
      staged.push(path);
      return path;
    },
    launch: (input) => void launched.push(input),
    openInstaller: async () => {},
    move: () => true,
    quit: () => void quits++,
    freeSpace: async () => 10e9,
    // Short waits run (a stall, a retry); the schedule is recorded, not run.
    setTimeout: ((callback: () => void, ms: number) => {
      if (schedule.has(ms)) {
        timers.push({ ms, callback });
        return { unref() {} } as unknown as ReturnType<typeof setTimeout>;
      }
      return setTimeout(callback, Math.min(ms, 20));
    }) as UpdaterOptions["setTimeout"],
    clearTimeout: (timer) => clearTimeout(timer),
    ...extra,
  };
  const updater = createUpdater(options);
  return {
    dir,
    updater,
    states,
    requests,
    timers,
    launched,
    staged,
    quits: () => quits,
    setAutomatic: (value: boolean) => void (automatic = value),
    last: () => states.at(-1) ?? updater.state(),
    /** Waits for whatever the updater started in the background. */
    settle: async () => {
      for (let index = 0; index < 50; index++) {
        const status = updater.state().status;
        if (status !== "downloading" && status !== "verifying" && !updater.state().checking) {
          await new Promise((resolve) => setTimeout(resolve, 10));
          if (updater.state().status === status) return;
        }
        await new Promise((resolve) => setTimeout(resolve, 20));
      }
    },
    cleanup: () => rm(dir, { recursive: true, force: true }),
  };
}

test("a newer release downloads by itself, verified and staged, ready to install on restart", async () => {
  const f = await fixture({});
  await f.updater.start();
  assert.deepEqual(f.timers.map((timer) => timer.ms), [FIRST_CHECK_MS]);
  await f.updater.check();
  await f.settle();
  const state = f.updater.state();
  assert.equal(state.status, "ready");
  assert.equal(state.version, "9.9.9");
  assert.equal(state.notesUrl, "https://haarness.example/notes");
  assert.equal(state.error, undefined);
  assert.ok(state.checkedAt);
  // Progress went up to 100, through verifying, without the sidebar's `asked`.
  const statuses = f.states.map((item) => item.status);
  assert.ok(statuses.includes("downloading") && statuses.includes("verifying"));
  assert.ok(f.states.some((item) => item.percent === 100));
  assert.ok(f.states.every((item) => !item.asked));
  const ready = JSON.parse(await readFile(join(f.dir, "ready.json"), "utf8"));
  assert.equal(ready.version, "9.9.9");
  assert.equal(ready.path, join(f.dir, "9.9.9", "Haarness.app"));
  // The next check is due in half an hour.
  assert.equal(f.timers.at(-1)?.ms, CHECK_EVERY_MS);
  // Restart quits; Haarness exiting starts the install and reopens it.
  f.updater.restart();
  assert.equal(f.updater.state().status, "installing");
  assert.equal(f.quits(), 1);
  f.updater.atExit();
  assert.deepEqual(f.launched, [{ staged: ready.path, version: "9.9.9", relaunch: true, result: join(f.dir, "install-result.json") }]);
  await f.cleanup();
});

test("the same or an older release leaves Haarness up to date", async () => {
  for (const version of ["0.1.5", "0.1.4", "0.1.5-preview.9"]) {
    const f = await fixture({ version });
    await f.updater.check();
    assert.equal(f.updater.state().status, "idle", version);
    assert.ok(f.updater.state().checkedAt);
    assert.equal(f.requests.length, 1);
    await f.cleanup();
  }
});

test("a failed check keeps what was found, says why, and tries again sooner", async () => {
  const f = await fixture({ status: 503 });
  await f.updater.check();
  assert.equal(f.updater.state().error?.code, "service");
  assert.equal(f.updater.state().checking, false);
  assert.equal(f.timers.at(-1)?.ms, RETRY_AFTER_MS[0]);
  await f.updater.check();
  assert.equal(f.timers.at(-1)?.ms, RETRY_AFTER_MS[1]);
  await f.cleanup();
  // No answer at all reads as offline; an answer that is no manifest, as the service.
  const offline = await fixture({}, {
    fetch: async () => {
      throw new TypeError("fetch failed");
    },
  });
  await offline.updater.check();
  assert.equal(offline.updater.state().error?.code, "offline");
  await offline.cleanup();
  const garbage = await fixture({ body: "<html>maintenance</html>" });
  await garbage.updater.check();
  assert.equal(garbage.updater.state().error?.code, "service");
  assert.equal(garbage.updater.state().status, "idle");
  await garbage.cleanup();
});

test("checks asked for at once share one request", async () => {
  const f = await fixture({}, { automatic: () => false });
  await Promise.all([f.updater.check(), f.updater.check(), f.updater.check()]);
  assert.equal(f.requests.length, 1);
  assert.equal(f.updater.state().status, "available");
  await f.cleanup();
});

test("an interrupted download resumes where it stopped, from the same file", async () => {
  let tries = 0;
  const f = await fixture({
    edge: (request) => {
      tries++;
      if (tries === 1)
        return new Response(stream([bytes.subarray(0, 30_000)], new Error("connection reset")), { status: 200, headers: { etag: '"v1"' } });
      assert.equal(request.range, "bytes=30000-");
      assert.equal(request.ifRange, '"v1"');
      return new Response(stream([bytes.subarray(30_000)]), {
        status: 206,
        headers: { etag: '"v1"', "content-range": `bytes 30000-${bytes.length - 1}/${bytes.length}` },
      });
    },
  });
  await f.updater.check();
  await f.settle();
  assert.equal(tries, 2);
  assert.equal(f.updater.state().status, "ready");
  await f.cleanup();
});

test("a file that changed meanwhile starts over, and one with no ETag is not continued", async () => {
  let tries = 0;
  const f = await fixture({
    edge: (request) => {
      tries++;
      if (tries === 1) return new Response(stream([bytes.subarray(0, 30_000)], new Error("reset")), { status: 200 });
      // No ETag the first time, so nothing to continue from.
      assert.equal(request.range, undefined);
      return whole();
    },
  });
  await f.updater.check();
  await f.settle();
  assert.equal(f.updater.state().status, "ready");
  await f.cleanup();
});

test("damaged bytes from the release domain are fetched again from the service itself", async () => {
  const damaged = Buffer.from(bytes);
  damaged[100] ^= 1;
  const f = await fixture({ edge: () => new Response(stream([damaged]), { status: 200, headers: { etag: '"x"' } }) });
  await f.updater.check();
  await f.settle();
  assert.equal(f.updater.state().status, "ready");
  assert.deepEqual(
    f.requests.filter((request) => !request.url.endsWith("/desktop")).map((request) => new URL(request.url).host),
    ["dl.haarness.example", "haarness.example"],
  );
  await f.cleanup();
});

test("a download damaged everywhere is not staged; it stays available with the reason", async () => {
  const damaged = Buffer.from(bytes);
  damaged[0] ^= 1;
  const bad = () => new Response(stream([damaged]), { status: 200 });
  const f = await fixture({ edge: bad, service: bad });
  await f.updater.check();
  await f.settle();
  assert.equal(f.updater.state().status, "available");
  assert.equal(f.updater.state().error?.code, "damaged");
  assert.equal(f.staged.length, 0);
  assert.equal(await stat(join(f.dir, "ready.json")).catch(() => null), null);
  // Nothing left half-written to resume from.
  assert.deepEqual(await readdir(join(f.dir, "9.9.9")), []);
  await f.cleanup();
});

test("a download larger than the release says is cut off as damaged", async () => {
  const big = () => new Response(stream([bytes, Buffer.from("more")]), { status: 200 });
  const f = await fixture({ edge: big, service: big });
  await f.updater.check();
  await f.settle();
  assert.equal(f.updater.state().error?.code, "damaged");
  await f.cleanup();
});

test("a download that stalls is abandoned and resumed", async () => {
  let tries = 0;
  const f = await fixture(
    {
      edge: () => {
        tries++;
        if (tries > 1) return whole();
        // Sends a little, then nothing.
        return new Response(
          new ReadableStream({
            start(controller) {
              controller.enqueue(new Uint8Array(bytes.subarray(0, 100)));
            },
          }),
          { status: 200, headers: { etag: '"v1"' } },
        );
      },
    },
  );
  await f.updater.check();
  await f.settle();
  assert.equal(tries, 2);
  assert.equal(f.updater.state().status, "ready");
  await f.cleanup();
});

test("without room on disk nothing downloads, and the window says why", async () => {
  const f = await fixture({}, { freeSpace: async () => 1000 });
  await f.updater.check();
  await f.settle();
  assert.equal(f.updater.state().status, "available");
  assert.equal(f.updater.state().error?.code, "space");
  assert.equal(f.requests.length, 1);
  await f.cleanup();
});

test("automatic downloads off: the update waits for the person, whose download shows its progress", async () => {
  const f = await fixture({});
  f.setAutomatic(false);
  await f.updater.check();
  assert.equal(f.updater.state().status, "available");
  assert.equal(f.requests.length, 1);
  const download = f.updater.download(true);
  assert.equal(f.updater.state().asked, true);
  await download;
  assert.equal(f.updater.state().status, "ready");
  // Turning it on later downloads at once.
  const g = await fixture({});
  g.setAutomatic(false);
  await g.updater.check();
  g.setAutomatic(true);
  g.updater.automaticChanged();
  await g.settle();
  assert.equal(g.updater.state().status, "ready");
  await f.cleanup();
  await g.cleanup();
});

test("a staged update survives a restart of Haarness, and a withdrawn one is dropped", async () => {
  const first = await fixture({});
  await first.updater.check();
  await first.settle();
  const second = await fixture({ version: "9.9.8" }, { dir: first.dir });
  await second.updater.start();
  assert.equal(second.updater.state().status, "ready");
  assert.equal(second.updater.state().version, "9.9.9");
  // 9.9.9 was withdrawn: the channel offers 9.9.8 again.
  await second.updater.check();
  await second.settle();
  assert.equal(second.updater.state().version, "9.9.8");
  assert.equal(second.updater.state().status, "ready");
  assert.equal(await stat(join(first.dir, "9.9.9", "Haarness.app")).catch(() => null), null);
  await first.cleanup();
});

test("a newer release than the one staged downloads behind it; the window keeps its restart", async () => {
  const first = await fixture({ version: "9.9.8" });
  await first.updater.check();
  await first.settle();
  const second = await fixture({ version: "9.9.9" }, { dir: first.dir });
  await second.updater.start();
  await second.updater.check();
  await second.settle();
  assert.ok(second.states.every((state) => state.status === "ready"), JSON.stringify(second.states.map((state) => state.status)));
  assert.equal(second.updater.state().version, "9.9.9");
  assert.equal(await stat(join(first.dir, "9.9.8", "Haarness.app")).catch(() => null), null);
  await first.cleanup();
});

test("the next launch cleans up what an install left, and says when it failed", async () => {
  const dir = await mkdtemp(join(tmpdir(), "haarness-updater-"));
  await mkdir(join(dir, "0.1.4"), { recursive: true });
  await mkdir(join(dir, "0.1.5"), { recursive: true });
  await mkdir(join(dir, "9.9.9", "Haarness.app"), { recursive: true });
  await writeFile(join(dir, "ready.json"), JSON.stringify({ version: "9.9.9", path: join(dir, "9.9.9", "Haarness.app") }));
  await writeFile(join(dir, "install-result.json"), JSON.stringify({ version: "9.9.9", ok: false, reason: "could not move the running version aside" }));
  const f = await fixture({}, { dir });
  await f.updater.start();
  assert.deepEqual((await readdir(dir)).sort(), ["9.9.9", "ready.json"]);
  assert.equal(f.updater.state().status, "ready");
  assert.equal(f.updater.state().error?.code, "install");
  // A check does not hide that the install failed; a restart to try again does.
  await f.updater.check();
  assert.equal(f.updater.state().error?.code, "install");
  f.updater.restart();
  assert.equal(f.updater.state().error, undefined);
  await rm(dir, { recursive: true, force: true });
  // Updated since: the old folders go, and nothing is offered.
  const later = await mkdtemp(join(tmpdir(), "haarness-updater-"));
  await mkdir(join(later, "9.9.9", "Haarness.app"), { recursive: true });
  await writeFile(join(later, "ready.json"), JSON.stringify({ version: "9.9.9", path: join(later, "9.9.9", "Haarness.app") }));
  await writeFile(join(later, "install-result.json"), JSON.stringify({ version: "9.9.9", ok: true }));
  const updated = await fixture({}, { dir: later, version: "9.9.9" });
  await updated.updater.start();
  assert.deepEqual(await readdir(later), []);
  assert.equal(updated.updater.state().status, "idle");
  assert.equal(updated.updater.state().error, undefined);
  await rm(later, { recursive: true, force: true });
});

test("a quit that a window kept back leaves the update ready; a plain quit installs it quietly", async () => {
  const f = await fixture({});
  await f.updater.check();
  await f.settle();
  f.updater.restart();
  f.updater.quitCancelled();
  assert.equal(f.updater.state().status, "ready");
  f.updater.atExit();
  assert.equal(f.launched.length, 1);
  assert.equal(f.launched[0].relaunch, false);
  // Exiting twice starts one install.
  f.updater.atExit();
  assert.equal(f.launched.length, 1);
  await f.cleanup();
});

test("the Windows setup runs only after Restart, never on a plain quit", async () => {
  const install: Install = { method: "restart", kind: "exe", target: "C:\\Haarness" };
  const exe = (version: string) => {
    const value = manifest(version);
    value.manifest.products.desktop.artifacts[0] = {
      ...value.manifest.products.desktop.artifacts[0],
      os: "windows",
      arch: "x64",
      kind: "exe",
      file: `Haarness-${version}-win-x64.exe`,
    };
    return value;
  };
  const quiet = await fixture({ body: exe("9.9.9") }, { install, os: "windows", arch: "x64", stage: async ({ file }) => file });
  await quiet.updater.check();
  await quiet.settle();
  assert.equal(quiet.updater.state().status, "ready");
  quiet.updater.atExit();
  assert.equal(quiet.launched.length, 0);
  const asked = await fixture({ body: exe("9.9.9") }, { install, os: "windows", arch: "x64", stage: async ({ file }) => file, dir: quiet.dir });
  await asked.updater.start();
  asked.updater.restart();
  asked.updater.atExit();
  assert.equal(asked.launched.length, 1);
  assert.ok(asked.launched[0].staged.endsWith("Haarness-9.9.9-win-x64.exe"));
  await quiet.cleanup();
});

test("a copy that cannot replace itself offers the installer; one off checks nothing", async () => {
  const opened: string[] = [];
  const dmg = manifest("9.9.9");
  dmg.manifest.products.desktop.artifacts[0].kind = "dmg";
  const f = await fixture({ body: dmg }, { install: { method: "installer", kind: "dmg" }, openInstaller: async (file) => void opened.push(file) });
  await f.updater.check();
  await f.settle();
  // Never downloaded by itself.
  assert.equal(f.updater.state().status, "available");
  assert.equal(f.updater.state().method, "installer");
  await f.updater.openInstaller();
  assert.equal(f.updater.state().status, "ready");
  assert.equal(opened.length, 1);
  assert.ok(opened[0].endsWith("Haarness-9.9.9-mac-arm64.zip"));
  await f.cleanup();
  const off = await fixture({}, { install: { method: "off" } });
  await off.updater.start();
  await off.updater.check();
  assert.equal(off.requests.length, 0);
  assert.equal(off.timers.length, 0);
  assert.equal(off.updater.state().method, "off");
  await off.cleanup();
});

test("a release that has no file for this computer is not offered", async () => {
  const f = await fixture({}, { arch: "x64", os: "linux", install: { method: "restart", kind: "AppImage", target: "/tmp/Haarness.AppImage" } });
  await f.updater.check();
  assert.equal(f.updater.state().status, "idle");
  await f.cleanup();
});

test("errors read as the window's reasons", () => {
  assert.equal(updateError(new UpdateError("damaged", "x")).code, "damaged");
  assert.equal(updateError(Object.assign(new Error("disk"), { code: "ENOSPC" })).code, "space");
  assert.equal(updateError(Object.assign(new Error("no"), { code: "EACCES" })).code, "write");
  assert.equal(updateError(new Error("net::ERR_INTERNET_DISCONNECTED")).code, "offline");
  assert.equal(updateError(new TypeError("fetch failed")).code, "offline");
  assert.equal(updateError(Object.assign(new Error("t"), { name: "TimeoutError" })).code, "offline");
  assert.equal(updateError(new Error("odd")).code, "unknown");
});

/** A body that sends its first bytes, then waits for `release` before the rest. */
function held(file: Buffer) {
  let release!: () => void;
  const gate = new Promise<void>((resolve) => (release = resolve));
  let sent = 0;
  const body = new ReadableStream<Uint8Array>({
    async pull(controller) {
      if (sent === 0) {
        sent = 1;
        controller.enqueue(new Uint8Array(file.subarray(0, 1000)));
        return;
      }
      await gate;
      controller.enqueue(new Uint8Array(file.subarray(1000)));
      controller.close();
    },
  });
  return { response: new Response(body, { status: 200, headers: { etag: '"h"' } }), release };
}

test("failed downloads back off to the half hour; a damaged file is not fetched again until the release changes", async () => {
  const broken = () => new Response(stream([bytes.subarray(0, 10)], new Error("reset")), { status: 200 });
  const f = await fixture({ edge: broken, service: broken });
  await f.updater.check();
  await f.settle();
  assert.equal(f.updater.state().error?.code, "offline");
  const delays = [f.timers.at(-1)?.ms];
  for (let round = 0; round < 4; round++) {
    await f.updater.check();
    await f.settle();
    delays.push(f.timers.at(-1)?.ms);
  }
  // Each round's checks succeed; the download's own failures set the pace.
  assert.deepEqual(delays, [...RETRY_AFTER_MS, CHECK_EVERY_MS, CHECK_EVERY_MS]);
  await f.cleanup();

  const damaged = Buffer.from(bytes);
  damaged[7] ^= 1;
  const bad = () => new Response(stream([damaged]), { status: 200 });
  const g = await fixture({ edge: bad, service: bad });
  await g.updater.check();
  await g.settle();
  assert.equal(g.updater.state().error?.code, "damaged");
  const fetched = () => g.requests.filter((request) => !request.url.endsWith("/desktop")).length;
  const before = fetched();
  for (let round = 0; round < 3; round++) {
    await g.updater.check();
    await g.settle();
  }
  assert.equal(fetched(), before, "a refused file is not downloaded again by itself");
  assert.equal(g.updater.state().error?.code, "damaged");
  assert.equal(g.timers.at(-1)?.ms, CHECK_EVERY_MS);
  // The person asks: it is tried again.
  await g.updater.download(true);
  assert.ok(fetched() > before);
  await g.cleanup();
});

test("a release withdrawn while it downloads is neither staged nor installed", async () => {
  const hold = held(bytes);
  const served: Served = { edge: () => hold.response };
  const f = await fixture(served);
  await f.updater.check();
  assert.equal(f.updater.state().status, "downloading");
  // Withdrawn: the channel offers this version again.
  served.version = "0.1.5";
  await f.updater.check();
  hold.release();
  await f.settle();
  assert.equal(f.updater.state().status, "idle");
  assert.equal(f.staged.length, 0);
  assert.equal(await stat(join(f.dir, "ready.json")).catch(() => null), null);
  assert.equal(await stat(join(f.dir, "9.9.9")).catch(() => null), null);
  f.updater.atExit();
  assert.deepEqual(f.launched, []);
  await f.cleanup();
});

test("a staged version is dropped when the channel no longer offers anything for this computer", async () => {
  const first = await fixture({});
  await first.updater.check();
  await first.settle();
  const second = await fixture({ status: 404 }, { dir: first.dir });
  await second.updater.start();
  assert.equal(second.updater.state().status, "ready");
  await second.updater.check();
  assert.equal(second.updater.state().status, "idle");
  second.updater.atExit();
  assert.deepEqual(second.launched, []);
  await first.cleanup();
});

test("a newer release while one downloads stops that one; only the newest is staged", async () => {
  const hold = held(bytes);
  const served: Served = { edge: () => hold.response };
  const f = await fixture(served);
  await f.updater.check();
  assert.equal(f.updater.state().version, "9.9.9");
  served.version = "9.9.10";
  served.edge = undefined;
  await f.updater.check();
  hold.release();
  await f.settle();
  assert.equal(f.updater.state().status, "ready");
  assert.equal(f.updater.state().version, "9.9.10");
  assert.deepEqual(f.staged.map((path) => path.includes("9.9.10")), [true]);
  assert.equal(await stat(join(f.dir, "9.9.9")).catch(() => null), null);
  await f.cleanup();
});

test("turning automatic downloads off stops one running by itself", async () => {
  const hold = held(bytes);
  const f = await fixture({ edge: () => hold.response });
  await f.updater.check();
  assert.equal(f.updater.state().status, "downloading");
  f.setAutomatic(false);
  f.updater.automaticChanged();
  assert.equal(f.updater.state().status, "available");
  hold.release();
  await f.settle();
  assert.equal(f.updater.state().status, "available");
  assert.equal(f.staged.length, 0);
  await f.cleanup();
});

test("a cancelled restart never leaves the reopening or the Windows setup armed", async () => {
  const install: Install = { method: "restart", kind: "exe", target: "C:\\Haarness" };
  const value = manifest("9.9.9");
  value.manifest.products.desktop.artifacts[0] = { ...value.manifest.products.desktop.artifacts[0], os: "windows", arch: "x64", kind: "exe" };
  const f = await fixture({ body: value }, { install, os: "windows", arch: "x64", stage: async ({ file }) => file });
  await f.updater.check();
  await f.settle();
  f.updater.restart();
  // A check ends while the window asks about unsaved edits.
  await f.updater.check();
  assert.equal(f.updater.state().status, "installing");
  f.updater.quitCancelled();
  assert.equal(f.updater.state().status, "ready");
  // The person quits later: the setup does not run unasked.
  f.updater.atExit();
  assert.deepEqual(f.launched, []);
  await f.cleanup();
});

test("a plain quit while the system shuts down installs nothing; the update waits for the next launch", async () => {
  const f = await fixture({});
  await f.updater.check();
  await f.settle();
  f.updater.systemShutdown();
  f.updater.atExit();
  assert.deepEqual(f.launched, []);
  assert.ok(await stat(join(f.dir, "ready.json")));
  await f.cleanup();
});

test("starting says what an earlier run left at once, and trusts no staged path outside its folder", async () => {
  const first = await fixture({});
  await first.updater.check();
  await first.settle();
  const second = await fixture({}, { dir: first.dir });
  await second.updater.start();
  assert.equal(second.states.at(-1)?.status, "ready");
  // A ready.json pointing elsewhere is dropped, and what it names is left alone.
  const outside = await mkdtemp(join(tmpdir(), "haarness-outside-"));
  await writeFile(join(first.dir, "ready.json"), JSON.stringify({ version: "9.9.9", path: join(first.dir, "..", outside.split("/").pop()!) }));
  const third = await fixture({}, { dir: first.dir });
  await third.updater.start();
  assert.equal(third.updater.state().status, "idle");
  assert.ok(await stat(outside));
  await rm(outside, { recursive: true, force: true });
  // A corrupt install result goes too.
  await writeFile(join(first.dir, "install-result.json"), "{not json");
  const fourth = await fixture({}, { dir: first.dir });
  await fourth.updater.start();
  assert.equal(await stat(join(first.dir, "install-result.json")).catch(() => null), null);
  await first.cleanup();
});

test("a rename Windows holds for a moment (an antivirus scan) is tried again; other failures are not", async () => {
  const { renameSoon } = await import("../src/main/updater");
  const dir = await mkdtemp(join(tmpdir(), "haarness-rename-"));
  await writeFile(join(dir, "a"), "x");
  // A real rename after waits: the waits are what a held file gets.
  const waits: number[] = [];
  await renameSoon(join(dir, "a"), join(dir, "b"), async (ms) => void waits.push(ms));
  assert.deepEqual(waits, []);
  assert.equal(await readFile(join(dir, "b"), "utf8"), "x");
  await assert.rejects(renameSoon(join(dir, "missing"), join(dir, "c"), async () => {}), { code: "ENOENT" });
  // Held the whole time: tried ten times, waiting a little longer each time, then the error.
  if (process.getuid?.() !== 0) {
    const { chmod } = await import("node:fs/promises");
    await mkdir(join(dir, "locked"));
    await chmod(join(dir, "locked"), 0o555);
    const held: number[] = [];
    await assert.rejects(renameSoon(join(dir, "b"), join(dir, "locked", "b"), async (ms) => void held.push(ms)), { code: "EACCES" });
    assert.deepEqual(held, [100, 200, 300, 400, 500, 600, 700, 800, 900]);
    await chmod(join(dir, "locked"), 0o755);
  }
  await rm(dir, { recursive: true, force: true });
});

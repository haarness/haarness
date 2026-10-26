import { createHash, type Hash } from "node:crypto";
import { createReadStream } from "node:fs";
import { mkdir, open, readdir, readFile, rename, rm, stat, writeFile } from "node:fs/promises";
import { basename, isAbsolute, join, relative, resolve } from "node:path";
import {
  channelsFor,
  compareVersions,
  parseRelease,
  pickArtifact,
  validVersion,
  type DesktopRelease,
  type ReleaseArtifact,
  type UpdateErrorCode,
  type UpdateState,
} from "../updates";

/**
 * How this copy of Desktop updates (see `UpdateState.method`), and which
 * release file it takes: `zip` for the macOS app, `AppImage`, the Windows
 * setup `exe`, or for an installer the person opens, that installer.
 */
export type Install =
  | { method: "restart"; kind: "zip" | "AppImage" | "exe"; target: string }
  | { method: "move"; kind: "zip" }
  | { method: "installer"; kind: "dmg" | "deb" | "AppImage" | "exe" }
  | { method: "off" };

/** A failure the window can name (see `UpdateErrorCode`). */
export class UpdateError extends Error {
  constructor(
    readonly code: UpdateErrorCode,
    message: string,
  ) {
    super(message);
  }
}

/** The first check waits until startup has settled. */
export const FIRST_CHECK_MS = 10_000;
/** Then one check every half hour (as Upstream Desktop does). */
export const CHECK_EVERY_MS = 30 * 60_000;
/** After a failed check or download: one minute, five, fifteen, then the half hour. */
export const RETRY_AFTER_MS = [60_000, 5 * 60_000, 15 * 60_000];
/** A download that brings nothing for this long has stalled; it resumes. */
export const STALL_MS = 30_000;
/** Each source gets this many tries, each resuming where the last stopped. */
const TRIES = 3;

interface Staged {
  version: string;
  /** The unpacked app, AppImage or installer that installing puts in place. */
  path: string;
  notesUrl?: string;
}
interface Found {
  release: DesktopRelease;
  artifact: ReleaseArtifact;
}
type Timer = ReturnType<typeof setTimeout>;

export interface UpdaterOptions {
  /** The running version. */
  version: string;
  /** This computer, as the release service names it. */
  os: "macos" | "linux" | "windows";
  arch: "arm64" | "x64";
  install: Install;
  /** `https://haarness.ai`, or a loopback HTTP service in tests. */
  releaseUrl: string;
  /** Where downloads, staged apps and their records live. */
  dir: string;
  fetch(url: string, init: RequestInit): Promise<Response>;
  /** Settings → About → Download updates automatically. */
  automatic(): boolean;
  emit(state: UpdateState): void;
  log(message: string): void;
  /** Verifies and prepares a downloaded file for install (unpacks the app,
   * checks its version and signature); returns what installing moves into
   * place. Throws an UpdateError. */
  stage(input: { file: string; version: string; folder: string; artifact: ReleaseArtifact }): Promise<string>;
  /** Starts the install as Haarness exits (see `atExit`). */
  launch(input: { staged: string; version: string; relaunch: boolean; result: string }): void;
  /** Puts a downloaded installer where the person finds it, and opens it. */
  openInstaller(file: string): Promise<void>;
  /** Moves the app to Applications, which relaunches it; false when it did not. */
  move(): boolean;
  quit(): void;
  /** Free bytes on the volume of `dir`, when that can be told. */
  freeSpace(dir: string): Promise<number | undefined>;
  now?(): number;
  setTimeout?(callback: () => void, ms: number): Timer;
  clearTimeout?(timer: Timer): void;
}

/** What a failure says to the person, from the error a step threw. */
export function updateError(error: unknown): { code: UpdateErrorCode; detail?: string } {
  if (error instanceof UpdateError) return { code: error.code, detail: error.message };
  const value = error as { code?: string; name?: string; message?: string; cause?: { code?: string } };
  const code = value?.code ?? value?.cause?.code;
  const detail = value?.message ? String(value.message) : String(error);
  if (code === "ENOSPC") return { code: "space", detail };
  if (code === "EACCES" || code === "EPERM" || code === "EROFS") return { code: "write", detail };
  if (
    value?.name === "AbortError" ||
    value?.name === "TimeoutError" ||
    value?.name === "TypeError" ||
    /^net::ERR_|ENOTFOUND|ECONNREFUSED|ECONNRESET|ETIMEDOUT|EAI_AGAIN|fetch failed/i.test(detail + " " + (code ?? ""))
  )
    return { code: "offline", detail };
  return { code: "unknown", detail };
}

/** A URL Desktop downloads from: HTTPS, or loopback HTTP when the release
 * service itself is one (tests). */
function allowed(url: string, releaseUrl: string) {
  try {
    const parsed = new URL(url);
    if (parsed.username || parsed.password) return false;
    if (parsed.protocol === "https:") return true;
    const loopback = (host: string) => host === "127.0.0.1" || host === "localhost" || host === "[::1]";
    return (
      parsed.protocol === "http:" &&
      loopback(parsed.hostname) &&
      new URL(releaseUrl).protocol === "http:" &&
      loopback(new URL(releaseUrl).hostname)
    );
  } catch {
    return false;
  }
}

async function readJson(file: string): Promise<Record<string, unknown> | undefined> {
  try {
    const value = JSON.parse(await readFile(file, "utf8"));
    return value && typeof value === "object" ? value : undefined;
  } catch {
    return undefined;
  }
}
async function exists(file: string) {
  return stat(file).then(
    () => true,
    () => false,
  );
}
async function hashFile(file: string, hash: Hash) {
  for await (const chunk of createReadStream(file)) hash.update(chunk as Buffer);
}

/** Writes `value` as JSON so a read never finds it half written. */
async function writeJson(file: string, value: unknown) {
  await writeFile(file + ".tmp", JSON.stringify(value));
  await rename(file + ".tmp", file);
}
/** Why a download stopped without failing: a newer release, a withdrawn
 * one, the automatic download turned off. It is not reported. */
class Stopped extends Error {}

/** The wait before trying again after `count` failures in a row. */
const retryAfter = (count: number) => RETRY_AFTER_MS[count - 1] ?? CHECK_EVERY_MS;

/**
 * Desktop's updater: checks the release service on a schedule, downloads the
 * newer version in the background (resuming an interrupted download, from the
 * release domain and then from the service itself), verifies it and stages
 * it; restarting, or on macOS and Linux simply quitting, installs it.
 * Everything that can fail does so quietly in the background and says why in
 * Settings → About; nothing here throws at the window.
 */
export function createUpdater(options: UpdaterOptions) {
  const now = options.now ?? Date.now;
  const later = options.setTimeout ?? setTimeout;
  const cancel = options.clearTimeout ?? clearTimeout;
  const install = options.install;
  const releaseUrl = options.releaseUrl.replace(/\/+$/, "");
  const ready = join(options.dir, "ready.json");
  const result = join(options.dir, "install-result.json");
  let state: UpdateState = { method: install.method, status: "idle", checking: false };
  let found: Found | undefined;
  let staged: Staged | undefined;
  let checking: Promise<void> | undefined;
  let downloading: { version: string; promise: Promise<void>; stop: AbortController; asked: boolean } | undefined;
  let timer: Timer | undefined;
  let checkFailures = 0;
  let downloadFailures = 0;
  /** A file that was damaged or not signed as it must be: not fetched
   * again by itself until the release changes it, only when asked. */
  let refused: { version: string; sha256: string } | undefined;
  let relaunch = false;
  let shuttingDown = false;
  let exited = false;
  let disposed = false;

  const set = (next: Partial<UpdateState>) => {
    state = { ...state, ...next };
    for (const key of Object.keys(next) as (keyof UpdateState)[])
      if (next[key] === undefined) delete state[key];
    options.emit({ ...state });
  };
  const kind = install.method === "off" ? undefined : install.kind;
  const restartable = install.method === "restart";
  /** Quitting to install: nothing changes what the window shows until it
   * is gone, or the quit is called off. */
  const installing = () => state.status === "installing";
  const isRefused = (target: Found) =>
    refused?.version === target.release.version && refused.sha256 === target.artifact.sha256;

  function schedule(ms: number) {
    if (disposed || install.method === "off") return;
    if (timer) cancel(timer);
    timer = later(() => {
      timer = undefined;
      void check();
    }, ms);
    (timer as { unref?: () => void }).unref?.();
  }

  async function forget() {
    if (!staged) return;
    const old = staged;
    staged = undefined;
    await rm(ready, { force: true }).catch(() => {});
    await removeStaged(old.path);
  }
  /** Whether `path` is something this updater made: inside its folder, or
   * the hidden AppImage it staged beside the one it replaces. */
  function ours(path: string) {
    const inside = relative(options.dir, resolve(path));
    return (
      (!!inside && !inside.startsWith("..") && !isAbsolute(inside)) ||
      /^\.[^/\\]+\.haarness-update$/.test(basename(path))
    );
  }
  async function removeStaged(path: string) {
    if (ours(path)) await rm(path, { recursive: true, force: true }).catch(() => {});
  }

  /** What an earlier run left: a finished or failed install, a staged
   * update, downloads of versions this one has passed. */
  async function init() {
    await mkdir(options.dir, { recursive: true });
    const outcome = await readJson(result);
    await rm(result, { force: true }).catch(() => {});
    if (outcome) {
      const version = String(outcome.version ?? "");
      if (outcome.ok === true) options.log(`Haarness update: installed ${version}`);
      else if (validVersion(version) && compareVersions(version, options.version) > 0) {
        options.log(`Haarness update: installing ${version} failed: ${String(outcome.reason ?? "")}`);
        state = { ...state, error: { code: "install", detail: String(outcome.reason ?? "") } };
      }
    }
    const saved = await readJson(ready);
    if (
      saved &&
      validVersion(saved.version) &&
      typeof saved.path === "string" &&
      ours(saved.path) &&
      compareVersions(saved.version, options.version) > 0 &&
      restartable &&
      (await exists(saved.path))
    ) {
      staged = {
        version: saved.version,
        path: saved.path,
        ...(typeof saved.notesUrl === "string" ? { notesUrl: saved.notesUrl } : {}),
      };
      state = { ...state, status: "ready", version: staged.version, ...(staged.notesUrl ? { notesUrl: staged.notesUrl } : {}) };
    } else if (saved) {
      await rm(ready, { force: true }).catch(() => {});
      if (typeof saved.path === "string") await removeStaged(saved.path);
    }
    for (const name of await readdir(options.dir).catch(() => [] as string[]))
      if (validVersion(name) && compareVersions(name, options.version) <= 0)
        await rm(join(options.dir, name), { recursive: true, force: true }).catch(() => {});
  }

  async function latest(): Promise<Found | undefined> {
    let best: Found | undefined;
    for (const channel of channelsFor(options.version)) {
      const response = await options.fetch(`${releaseUrl}/api/v1/releases/${channel}/desktop`, {
        headers: { accept: "application/json" },
        cache: "no-store",
        signal: AbortSignal.timeout(20_000),
      });
      // A channel with no release yet.
      if (response.status === 404) continue;
      if (!response.ok) throw new UpdateError("service", `The release service answered HTTP ${response.status}.`);
      let release: DesktopRelease | null;
      try {
        release = parseRelease(await response.json());
      } catch (error) {
        throw new UpdateError("service", error instanceof Error ? error.message : String(error));
      }
      if (!release || !kind) continue;
      const artifact = pickArtifact(release, { os: options.os, arch: options.arch, kind });
      if (!artifact || (!allowed(artifact.url, releaseUrl) && !allowed(service(artifact), releaseUrl))) continue;
      if (!best || compareVersions(release.version, best.release.version) > 0) best = { release, artifact };
    }
    return best;
  }
  const service = (artifact: ReleaseArtifact) =>
    `${releaseUrl}/api/v1/releases/artifacts/${encodeURIComponent(artifact.id)}`;

  /** Asks the release service for the newest version; one check at a time. */
  function check(): Promise<void> {
    if (install.method === "off" || disposed) return Promise.resolve();
    if (checking) return checking;
    checking = (async () => {
      set({ checking: true });
      try {
        const next = await latest();
        checkFailures = 0;
        found = next && compareVersions(next.release.version, options.version) > 0 ? next : undefined;
        // A staged version the service no longer offers (it offers an older
        // one, or none for this computer) was withdrawn.
        if (staged && (!next || compareVersions(next.release.version, staged.version) < 0)) {
          options.log(`Haarness update: ${staged.version} was withdrawn; ${next?.release.version ?? "nothing"} is offered`);
          await forget();
        }
        // So is one still downloading that the service no longer names.
        if (downloading && downloading.version !== found?.release.version)
          downloading.stop.abort(new Stopped(`${downloading.version} is no longer offered`));
        // New bytes, or another version, may be tried again by themselves.
        if (refused && !(found && isRefused(found))) refused = undefined;
        const clear = state.error?.code === "install" ? {} : { error: undefined };
        if (installing()) {
          // Quitting to install what is staged: nothing else to say.
        } else if (!found) {
          if (!staged)
            set({ status: "idle", version: undefined, percent: undefined, notesUrl: undefined, asked: undefined, ...clear });
          else set(clear);
        } else if (staged && staged.version === found.release.version) {
          set({ status: "ready", version: staged.version, notesUrl: staged.notesUrl, ...clear });
        } else if (downloading?.version === found.release.version) {
          // Downloading it already; a refusal or failure keeps its reason.
        } else if (!staged) {
          set({
            status: "available",
            version: found.release.version,
            notesUrl: found.release.notesUrl,
            percent: undefined,
            asked: undefined,
            ...(isRefused(found) ? {} : clear),
          });
        } else set(clear);
        set({ checking: false, checkedAt: now() });
        schedule(CHECK_EVERY_MS);
        // Newer than anything here: fetch it now, while the person works.
        if (found && restartable && options.automatic() && staged?.version !== found.release.version && !installing())
          void download(false);
      } catch (error) {
        checkFailures++;
        const reason = updateError(error);
        options.log(`Haarness update: checking failed (${reason.code}): ${reason.detail ?? ""}`);
        schedule(retryAfter(checkFailures));
        set({ checking: false, ...(state.error?.code === "install" ? {} : { error: reason }) });
      } finally {
        checking = undefined;
      }
    })();
    return checking;
  }

  /** Downloads `artifact` into `folder`, resuming a partial file; returns the
   * verified file. `stop` ends it at once, with no further tries. */
  async function fetchVerified(
    artifact: ReleaseArtifact,
    folder: string,
    progress: (fraction: number) => void,
    stop: AbortSignal,
  ) {
    const file = join(folder, artifact.file);
    const part = file + ".part";
    const meta = part + ".json";
    // Downloaded before, by a run that stopped before staging it.
    if (await exists(file)) {
      const hash = createHash("sha256");
      await hashFile(file, hash);
      if (hash.digest("hex") === artifact.sha256) return file;
      await rm(file, { force: true });
    }
    const sources = [...new Set([artifact.url, service(artifact)])].filter((url) => allowed(url, releaseUrl));
    let last: unknown = new UpdateError("service", "The release names no address Haarness may download from.");
    for (const source of sources) {
      for (let attempt = 1; attempt <= TRIES; attempt++) {
        if (stop.aborted) throw stop.reason;
        try {
          await fetchInto(source, part, meta, artifact, progress, stop);
          await rename(part, file);
          await rm(meta, { force: true });
          return file;
        } catch (error) {
          if (stop.aborted) throw stop.reason;
          last = error;
          const reason = updateError(error);
          options.log(`Haarness update: download from ${new URL(source).host} failed (${reason.code}): ${reason.detail ?? ""}`);
          if (reason.code === "space" || reason.code === "write" || disposed) throw error;
          // Damaged bytes from one source are not asked for again.
          if (reason.code === "damaged") {
            await rm(part, { force: true });
            await rm(meta, { force: true });
            break;
          }
          if (attempt < TRIES) await new Promise<void>((resolve) => later(resolve, attempt * 2_000));
        }
      }
    }
    throw last;
  }

  async function fetchInto(
    source: string,
    part: string,
    meta: string,
    artifact: ReleaseArtifact,
    progress: (fraction: number) => void,
    stop: AbortSignal,
  ) {
    let have = await stat(part).then(
      (info) => info.size,
      () => 0,
    );
    const saved = await readJson(meta);
    const etag = saved?.source === source && typeof saved.etag === "string" ? saved.etag : "";
    // Only a partial file the same source can continue is kept (as
    // install.sh does); If-Range has the server send the whole file instead
    // when it changed meanwhile.
    if (have > artifact.size || (have && !etag)) {
      await rm(part, { force: true });
      have = 0;
    }
    const stall = new AbortController();
    const stopped = () => stall.abort(stop.reason);
    stop.addEventListener("abort", stopped, { once: true });
    let idle: Timer | undefined;
    const watch = () => {
      if (idle) cancel(idle);
      idle = later(() => stall.abort(new UpdateError("offline", "The download stalled.")), STALL_MS);
    };
    watch();
    try {
      const response = await options.fetch(source, {
        headers: have ? { range: `bytes=${have}-`, "if-range": etag } : {},
        cache: "no-store",
        redirect: "follow",
        signal: stall.signal,
      });
      if (response.status === 416) {
        await rm(part, { force: true });
        throw new UpdateError("service", "The server refused to continue the download.");
      }
      const resumed = response.status === 206 && have > 0;
      if (resumed) {
        const range = /^bytes (\d+)-/.exec(response.headers.get("content-range") ?? "");
        if (!range || Number(range[1]) !== have) throw new UpdateError("service", "The server continued from another place.");
      } else if (response.status !== 200) throw new UpdateError("service", `The download answered HTTP ${response.status}.`);
      if (!response.body) throw new UpdateError("service", "The download had no body.");
      if (!resumed) have = 0;
      const tag = response.headers.get("etag");
      await writeFile(meta, JSON.stringify({ source, etag: tag ?? "" }));
      const hash = createHash("sha256");
      if (have) await hashFile(part, hash);
      const handle = await open(part, have ? "a" : "w");
      let received = have;
      try {
        const reader = response.body.getReader();
        // A stalled or stopped body ends here even when the fetch does not end it itself.
        stall.signal.addEventListener("abort", () => void reader.cancel(stall.signal.reason).catch(() => {}), { once: true });
        if (stall.signal.aborted) throw stall.signal.reason;
        for (;;) {
          let chunk: ReadableStreamReadResult<Uint8Array>;
          try {
            chunk = await reader.read();
          } catch (error) {
            // The connection broke mid-download: a network failure, resumed next try.
            if (stall.signal.aborted) throw stall.signal.reason;
            throw new UpdateError("offline", error instanceof Error ? error.message : String(error));
          }
          const { done, value } = chunk;
          if (done) break;
          watch();
          received += value.byteLength;
          if (received > artifact.size) throw new UpdateError("damaged", "The download is larger than the release says.");
          hash.update(value);
          await handle.write(value);
          progress(received / artifact.size);
        }
      } finally {
        await handle.close();
      }
      if (stall.signal.aborted) throw stall.signal.reason;
      if (received !== artifact.size) throw new UpdateError("offline", "The download ended early.");
      if (hash.digest("hex") !== artifact.sha256) throw new UpdateError("damaged", "The download's SHA-256 does not match the release.");
    } catch (error) {
      // An abort carries the reason it was given.
      if (stall.signal.aborted && (stall.signal.reason instanceof UpdateError || stall.signal.reason instanceof Stopped))
        throw stall.signal.reason;
      throw error;
    } finally {
      if (idle) cancel(idle);
      stop.removeEventListener("abort", stopped);
    }
  }

  /**
   * Downloads, verifies and stages the newer version. `asked`: the person
   * asked (Download, Update to …), so the sidebar shows the progress, and a
   * file refused before is tried again.
   */
  function download(asked: boolean): Promise<void> {
    if (!found || install.method === "off" || install.method === "move" || disposed) return Promise.resolve();
    const target = found;
    const version = target.release.version;
    if (staged?.version === version) return Promise.resolve();
    if (asked) refused = undefined;
    else if (isRefused(target)) return Promise.resolve();
    if (downloading?.version === version) {
      if (asked) downloading.asked = true;
      if (asked && !state.asked && state.version === version) set({ asked: true });
      return downloading.promise;
    }
    // One download at a time: one for another version stops first.
    const previous = downloading;
    previous?.stop.abort(new Stopped(`${version} supersedes ${previous.version}`));
    const stop = new AbortController();
    // With an older version staged, the newer one downloads behind it: the
    // window keeps offering the restart it has until this one is ready.
    const quiet = !!staged;
    // Assigned apart from its declaration: its own `finally` reads it.
    let promise!: Promise<void>;
    const announce = () => {
      if (!quiet && !installing())
        set({ status: "downloading", version, percent: 0, notesUrl: target.release.notesUrl, asked: asked || undefined, error: undefined });
    };
    // Said at once, so a click shows its download straight away.
    if (!previous) announce();
    promise = (async () => {
      if (previous) {
        await previous.promise.catch(() => {});
        if (stop.signal.aborted) return;
        announce();
      }
      const folder = join(options.dir, version);
      try {
        await mkdir(folder, { recursive: true });
        // The download, and on macOS the app it unpacks to, with room to spare.
        const needed = target.artifact.size * (kind === "zip" ? 4 : 1.2);
        const free = await options.freeSpace(options.dir).catch(() => undefined);
        if (free !== undefined && free < needed)
          throw new UpdateError("space", `The update needs ${Math.ceil(needed / 1e6)} MB free; ${Math.floor(free / 1e6)} MB are.`);
        let shown = -1;
        const file = await fetchVerified(
          target.artifact,
          folder,
          (fraction) => {
            const percent = Math.min(100, Math.floor(fraction * 100));
            if (percent !== shown && state.version === version && state.status === "downloading") {
              shown = percent;
              set({ percent });
            }
          },
          stop.signal,
        );
        // Withdrawn, or passed by a newer one, while it downloaded.
        const offered = () => !stop.signal.aborted && !disposed && found?.release.version === version;
        if (!offered()) throw stop.signal.reason ?? new Stopped(`${version} is no longer offered`);
        if (install.method === "installer") {
          staged = { version, path: file, notesUrl: target.release.notesUrl };
          downloadFailures = 0;
          set({ status: "ready", percent: undefined });
          return;
        }
        if (!quiet && !installing()) set({ status: "verifying", percent: undefined });
        const path = await options.stage({ file, version, folder, artifact: target.artifact });
        if (!offered()) {
          await removeStaged(path);
          throw stop.signal.reason ?? new Stopped(`${version} is no longer offered`);
        }
        const older = staged;
        staged = { version, path, notesUrl: target.release.notesUrl };
        await writeJson(ready, staged);
        if (older && older.path !== path) await removeStaged(older.path);
        downloadFailures = 0;
        options.log(`Haarness update: ${version} is ready`);
        if (!installing())
          set({ status: "ready", version, notesUrl: target.release.notesUrl, percent: undefined, error: undefined });
      } catch (error) {
        if (error instanceof Stopped || stop.signal.aborted) {
          options.log(`Haarness update: stopped downloading ${version}: ${error instanceof Error ? error.message : String(error)}`);
          // A version no longer offered leaves nothing behind.
          if (found?.release.version !== version) await rm(folder, { recursive: true, force: true }).catch(() => {});
          return;
        }
        const reason = updateError(error);
        options.log(`Haarness update: downloading ${version} failed (${reason.code}): ${reason.detail ?? ""}`);
        if (reason.code === "damaged" || reason.code === "signature")
          refused = { version, sha256: target.artifact.sha256 };
        else {
          // Tried again sooner, then every half hour: never in a tight loop.
          downloadFailures++;
          schedule(retryAfter(downloadFailures));
        }
        // Behind a staged version, a failure stays in the log: the restart
        // the window offers still works.
        if (!quiet && !installing()) set({ status: "available", version, percent: undefined, error: reason });
      } finally {
        if (downloading?.promise === promise) downloading = undefined;
      }
    })();
    downloading = { version, promise, stop, asked };
    return promise;
  }

  return {
    state: () => ({ ...state }),
    async start() {
      try {
        await init();
      } catch (error) {
        options.log(`Haarness update: could not read earlier updates: ${String(error)}`);
      }
      // The window shows what an earlier run left at once, not at the first check.
      options.emit({ ...state });
      schedule(FIRST_CHECK_MS);
    },
    check,
    download,
    /** Quits to install the staged version, and opens Haarness again. */
    restart() {
      if (!restartable || !staged || state.status !== "ready") return;
      relaunch = true;
      set({ status: "installing", error: undefined });
      options.quit();
    },
    /** A window kept its unsaved edits, so Haarness did not quit after all. */
    quitCancelled() {
      relaunch = false;
      if (installing()) set(staged ? { status: "ready", version: staged.version } : { status: "idle" });
    },
    /** The system is shutting down or logging out: a plain quit then installs
     * nothing, as being stopped halfway could leave no app at all. */
    systemShutdown() {
      shuttingDown = true;
    },
    /** Moves the app to Applications (it relaunches from there). */
    move() {
      if (install.method !== "move") return;
      try {
        if (!options.move()) set({ error: { code: "write", detail: "The app was not moved." } });
      } catch (error) {
        set({ error: updateError(error) });
      }
    },
    /** Downloads the installer if needed, then opens it for the person. */
    async openInstaller() {
      if (install.method !== "installer") return;
      if (!staged) await download(true);
      if (!staged) return;
      try {
        await options.openInstaller(staged.path);
      } catch (error) {
        set({ error: updateError(error) });
      }
    },
    /** Starts the staged install as Haarness exits: always after Restart;
     * on macOS and Linux also after a plain quit, which replaces the app
     * quietly. The Windows setup only runs when asked, never by surprise. */
    atExit() {
      if (exited) return;
      exited = true;
      disposed = true;
      if (timer) cancel(timer);
      downloading?.stop.abort(new Stopped("Haarness is quitting"));
      if (!restartable || !staged) return;
      if (!relaunch && (install.kind === "exe" || shuttingDown)) return;
      try {
        options.launch({ staged: staged.path, version: staged.version, relaunch, result });
        options.log(`Haarness update: installing ${staged.version}${relaunch ? " and reopening" : ""}`);
      } catch (error) {
        options.log(`Haarness update: could not start installing ${staged.version}: ${String(error)}`);
      }
    },
    /** The computer woke or came back online: a check that is due, or that
     * failed, happens soon instead of on its timer. */
    wake() {
      const last = state.checkedAt ?? 0;
      if (checkFailures || downloadFailures || now() - last >= CHECK_EVERY_MS) schedule(5_000);
    },
    /** The automatic download setting changed: on, a waiting version
     * downloads; off, one downloading by itself stops. */
    automaticChanged() {
      if (options.automatic()) {
        if (found && restartable && state.status === "available") void download(false);
        return;
      }
      if (downloading && !downloading.asked) {
        const version = downloading.version;
        downloading.stop.abort(new Stopped("automatic downloads were turned off"));
        if (!staged && !installing()) set({ status: "available", version, percent: undefined });
      }
    },
  };
}
export type Updater = ReturnType<typeof createUpdater>;

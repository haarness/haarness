import { execFile, spawn } from "node:child_process";
import { constants } from "node:fs";
import { access, chmod, copyFile, lstat, mkdir, readdir, rename, rm, statfs } from "node:fs/promises";
import { basename, dirname, extname, join, resolve, win32 } from "node:path";
import { UpdateError, type Install } from "./updater";

/** Runs a system tool; resolves with what it wrote, rejects when it fails. */
export function run(command: string, args: string[]) {
  return new Promise<{ stdout: string; stderr: string }>((done, fail) =>
    execFile(command, args, { maxBuffer: 4 << 20, timeout: 10 * 60_000 }, (error, stdout, stderr) =>
      error ? fail(Object.assign(error, { stderr })) : done({ stdout, stderr }),
    ),
  );
}

const writable = (path: string) =>
  access(path, constants.W_OK).then(
    () => true,
    () => false,
  );

/**
 * How this copy updates. A packaged macOS app replaces its own bundle when it
 * may write it and its folder; run from a disk image or translocated, it has
 * to be moved to Applications first; anywhere it may not write, the person
 * opens an installer. On Linux the AppImage replaces itself (the .deb, owned
 * by root, takes a new .deb), and Windows runs its setup. A development build
 * does not update, unless a test names the app it stands in for.
 */
export async function detectInstall(input: {
  platform: NodeJS.Platform;
  execPath: string;
  packaged: boolean;
  env: NodeJS.ProcessEnv;
  /** A test's stand-in for the installed app (bundle or AppImage). */
  testTarget?: string;
}): Promise<Install> {
  if (!input.packaged && !input.testTarget) return { method: "off" };
  if (input.platform === "darwin") {
    const bundle = input.testTarget ?? resolve(input.execPath, "../../..");
    if (extname(bundle) !== ".app") return { method: "off" };
    if (bundle.includes("/AppTranslocation/")) return { method: "move", kind: "zip" };
    if ((await writable(dirname(bundle))) && (await writable(bundle)))
      return { method: "restart", kind: "zip", target: bundle };
    // A disk image mounts read-only under /Volumes.
    if (bundle.startsWith("/Volumes/")) return { method: "move", kind: "zip" };
    return { method: "installer", kind: "dmg" };
  }
  if (input.platform === "win32") return { method: "restart", kind: "exe", target: win32.dirname(input.execPath) };
  if (input.platform === "linux") {
    const image = input.testTarget ?? input.env.APPIMAGE;
    if (image) {
      if ((await writable(dirname(image))) && (await writable(image)))
        return { method: "restart", kind: "AppImage", target: image };
      return { method: "installer", kind: "AppImage" };
    }
    return { method: "installer", kind: input.execPath.startsWith("/opt/") ? "deb" : "AppImage" };
  }
  return { method: "off" };
}

/** The Apple team that signed an app: undefined for an ad hoc signature
 * (a local build), null when its signature cannot be read at all. */
export async function macTeam(bundle: string): Promise<string | undefined | null> {
  try {
    const { stderr } = await run("/usr/bin/codesign", ["-dv", "--verbose=2", bundle]);
    const team = /^TeamIdentifier=(.+)$/m.exec(stderr)?.[1]?.trim();
    if (!team || team === "not set") return undefined;
    return /^[A-Z0-9]{10}$/.test(team) ? team : null;
  } catch {
    return null;
  }
}

/**
 * Unpacks a verified macOS ZIP and checks the app in it before anything uses
 * it: it is the version the release names, its signature verifies, and when
 * the running app is a team's, it is signed through Apple's certificate chain
 * by that same team (the one check that does not rest on the release
 * service). A running app whose signature cannot be read accepts nothing.
 * Returns the staged app; the ZIP goes once unpacked.
 */
export async function stageMacApp(input: { file: string; version: string; folder: string; team?: string | null }) {
  if (input.team === null)
    throw new UpdateError("signature", "This app's own signature could not be read, so no update can be checked against it.");
  const unpacked = join(input.folder, "unpacked");
  await rm(unpacked, { recursive: true, force: true });
  await mkdir(unpacked, { recursive: true });
  try {
    await run("/usr/bin/ditto", ["-x", "-k", input.file, unpacked]);
  } catch (error) {
    throw new UpdateError("write", `The update could not be unpacked: ${(error as { stderr?: string }).stderr || String(error)}`);
  }
  const apps = (await readdir(unpacked)).filter((name) => name.endsWith(".app"));
  if (apps.length !== 1) throw new UpdateError("damaged", "The update's archive holds no app.");
  const app = join(unpacked, apps[0]);
  let version = "";
  try {
    version = (
      await run("/usr/bin/plutil", ["-extract", "CFBundleShortVersionString", "raw", "-o", "-", join(app, "Contents/Info.plist")])
    ).stdout.trim();
  } catch {}
  if (version !== input.version)
    throw new UpdateError("damaged", `The update's archive holds version ${version || "unknown"}, not ${input.version}.`);
  // Not downloaded by a browser, so not quarantined; a copy that is would ask on first launch.
  await run("/usr/bin/xattr", ["-dr", "com.apple.quarantine", app]).catch(() => {});
  const requirement = input.team
    ? ["-R", `=anchor apple generic and certificate leaf[subject.OU] = "${input.team}"`]
    : [];
  try {
    await run("/usr/bin/codesign", ["--verify", "--deep", "--strict", ...requirement, app]);
  } catch (error) {
    const detail = (error as { stderr?: string }).stderr || String(error);
    throw new UpdateError(
      "signature",
      input.team ? `The new app is not signed by Apple-certified team ${input.team}: ${detail}` : `The new app's signature does not verify: ${detail}`,
    );
  }
  const staged = join(input.folder, "Haarness.app");
  await rm(staged, { recursive: true, force: true });
  await rename(app, staged);
  await rm(unpacked, { recursive: true, force: true });
  await rm(input.file, { force: true });
  return staged;
}

/** Puts a verified AppImage beside the one it replaces, hidden, so installing
 * is one rename on the same filesystem. */
export async function stageAppImage(input: { file: string; target: string }) {
  const staged = join(dirname(input.target), `.${basename(input.target)}.haarness-update`);
  await copyFile(input.file, staged + ".tmp");
  await chmod(staged + ".tmp", 0o755);
  await rename(staged + ".tmp", staged);
  await rm(input.file, { force: true });
  return staged;
}

/**
 * Installs a staged app (macOS bundle or AppImage) once Haarness has exited,
 * then opens it again when asked. Run detached as
 * `sh -c HELPER sh <pid> <target> <staged> <result> <version> <relaunch> <command…>`.
 * The running copy is moved aside, never deleted, until the new one is in
 * place (an AppImage, a file, is replaced by one rename): any failed step
 * puts both back and opens the old one. It ignores
 * the hangup and termination a logout sends, which could otherwise stop it
 * between its two renames. It writes what happened to <result> for the next
 * launch to read.
 */
export const HELPER = `
pid=$1 target=$2 staged=$3 result=$4 version=$5 relaunch=$6
shift 6
trap '' HUP INT TERM
report() {
  printf '{"version":"%s","ok":%s,"reason":"%s"}\\n' "$version" "$1" "$2" > "$result.tmp" && mv -f "$result.tmp" "$result"
}
i=0
while kill -0 "$pid" 2>/dev/null; do
  i=$((i + 1))
  if [ "$i" -gt 1200 ]; then report false "Haarness did not exit"; exit 1; fi
  sleep 0.1
done
parent=$(dirname "$target")
name=$(basename "$target")
incoming="$parent/.$name.incoming.$$"
backup="$parent/.$name.previous.$$"
ok=false
reason=''
if [ ! -d "$target" ]; then
  # A file (the AppImage): one rename replaces it, and the copy still running keeps its own.
  if ! mv "$staged" "$incoming" 2>/dev/null; then
    rm -f "$incoming"
    reason='could not copy the new version beside the old one'
  elif ! mv -f "$incoming" "$target" 2>/dev/null; then
    mv "$incoming" "$staged" 2>/dev/null || rm -f "$incoming"
    reason='could not put the new version in place'
  else
    ok=true
  fi
elif ! mv "$staged" "$incoming" 2>/dev/null; then
  rm -rf "$incoming"
  reason='could not copy the new version beside the old one'
elif ! mv "$target" "$backup" 2>/dev/null; then
  mv "$incoming" "$staged" 2>/dev/null || rm -rf "$incoming"
  reason='could not move the running version aside'
elif ! mv "$incoming" "$target" 2>/dev/null; then
  mv "$backup" "$target"
  mv "$incoming" "$staged" 2>/dev/null || rm -rf "$incoming"
  reason='could not put the new version in place'
else
  ok=true
  touch "$target" 2>/dev/null
  rm -rf "$backup"
fi
if [ "$ok" = true ]; then report true ''; else report false "$reason"; fi
if [ "$relaunch" = 1 ] && [ $# -gt 0 ]; then
  nohup "$@" >/dev/null 2>&1 &
fi
`;

/** Starts installing as Haarness exits (see `HELPER`); Windows runs the setup,
 * which closes what is left of Haarness itself and offers to open it. */
export function launchInstall(input: {
  platform: NodeJS.Platform;
  target: string;
  staged: string;
  version: string;
  relaunch: boolean;
  result: string;
  env: NodeJS.ProcessEnv;
  /** What opens Haarness again; a test records it instead. */
  reopen?: string[];
  /** The running app's own arguments; the reopened Linux app gets its flags (`--no-sandbox`). */
  argv?: string[];
}) {
  if (input.platform === "win32") {
    // As electron-updater runs it: an update into the installed folder,
    // without the wizard, opening Haarness when done. The setup waits for
    // what is left of Haarness to exit.
    spawn(input.staged, ["--updated", "/S", "--force-run"], { detached: true, stdio: "ignore" }).unref();
    return;
  }
  const env = { ...input.env };
  let command: string[];
  if (input.platform === "darwin") {
    // `open` starts it as the Finder would; Haarness's own settings (a
    // test's data folders) go with it.
    const keep = Object.entries(env)
      .filter(([key, value]) => key.startsWith("HAARNESS_") && value !== undefined)
      .flatMap(([key, value]) => ["--env", `${key}=${value}`]);
    command = ["/usr/bin/open", "-n", ...keep, input.target];
  } else {
    // The old AppImage's runtime set these for itself; the new one sets its own.
    for (const key of ["APPDIR", "APPIMAGE", "ARGV0", "OWD"]) delete env[key];
    command = [input.target, ...(input.argv ?? []).slice(1).filter((argument) => /^--[a-z]/.test(argument))];
  }
  spawn(
    "/bin/sh",
    ["-c", HELPER, "sh", String(process.pid), input.target, input.staged, input.result, input.version, input.relaunch ? "1" : "0", ...(input.reopen ?? command)],
    { detached: true, stdio: "ignore", env },
  ).unref();
}

/** Copies a downloaded installer into Downloads (beside any older copy) and opens it. */
export async function openInstaller(file: string, downloads: string, openPath: (path: string) => Promise<string>) {
  await mkdir(downloads, { recursive: true });
  const extension = extname(file);
  const stem = basename(file, extension);
  let target = "";
  for (let index = 0; index < 100; index++) {
    const candidate = join(downloads, index ? `${stem} (${index})${extension}` : basename(file));
    try {
      await copyFile(file, candidate, constants.COPYFILE_EXCL);
      target = candidate;
      break;
    } catch (error) {
      if ((error as { code?: string }).code !== "EEXIST") throw error;
    }
  }
  if (!target) throw new UpdateError("write", "Downloads has too many copies of this installer.");
  if (extension === ".AppImage") await chmod(target, 0o755);
  const problem = await openPath(target);
  if (problem) throw new UpdateError("write", problem);
}

/** Free bytes on the volume holding `path`. */
export async function freeSpace(path: string) {
  const info = await statfs(path);
  return Number(info.bavail) * Number(info.bsize);
}

/**
 * What an install that was stopped halfway (a logout, a crash) left beside
 * the app: the old copy moved aside, a new one not yet in place. Each is
 * removed once it is ten minutes old, so an install still running is left
 * alone.
 */
export async function removeLeftovers(target: string, now = Date.now()) {
  const name = basename(target).replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const pattern = new RegExp(`^\\.${name}\\.(previous|incoming)\\.\\d+$`);
  const folder = dirname(target);
  for (const entry of await readdir(folder).catch(() => [] as string[])) {
    if (!pattern.test(entry)) continue;
    const path = join(folder, entry);
    const info = await lstat(path).catch(() => undefined);
    if (info && now - info.mtimeMs > 10 * 60_000) await rm(path, { recursive: true, force: true }).catch(() => {});
  }
}

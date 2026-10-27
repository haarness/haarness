import { execFile, spawn } from "node:child_process";
import { constants, existsSync } from "node:fs";
import { access, chmod, copyFile, lstat, mkdir, readdir, rm, statfs } from "node:fs/promises";
import { basename, dirname, extname, join, resolve, win32 } from "node:path";
import { UpdateError, renameSoon, type Install } from "./updater";

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
  await renameSoon(app, staged);
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
  await renameSoon(staged + ".tmp", staged);
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

/**
 * Installs a staged Windows setup once Haarness has exited, as `HELPER` does
 * on macOS and Linux; run detached by Windows PowerShell 5.1 (any Windows 10
 * or 11) with its inputs in the environment, so nothing needs quoting:
 * HAARNESS_UPDATE_PID (Haarness), _SETUP, _RESULT, _VERSION, _BIN (Desktop's
 * `resources\bin`), _RECORDS (the servers' records, `<cache>\workspaces`) and
 * _APP (Haarness.exe, opened again when the setup fails).
 *
 * The setup ends every program running from the install folder at once,
 * Desktop's own servers (`resources\bin\haarness.exe`) included: Windows
 * cannot replace a running program. So those servers are first asked to stop
 * on their own terms (`POST /v1/shutdown`, as `haarness serve --stop` does),
 * for up to 15 seconds; servers of a standalone CLI elsewhere are not touched.
 * Then the setup runs as electron-updater runs it (`--updated /S --force-run`:
 * into the installed folder, no wizard, Haarness opened when done), and what
 * it answered is written to _RESULT for the next launch.
 */
export const WINDOWS_HELPER = `
$ErrorActionPreference = 'Continue'
function Report([bool]$ok, [string]$reason) {
  $json = @{ version = $env:HAARNESS_UPDATE_VERSION; ok = $ok; reason = $reason } | ConvertTo-Json -Compress
  Set-Content -LiteralPath ($env:HAARNESS_UPDATE_RESULT + '.tmp') -Value $json -Encoding ascii
  Move-Item -Force -LiteralPath ($env:HAARNESS_UPDATE_RESULT + '.tmp') -Destination $env:HAARNESS_UPDATE_RESULT
}
$deadline = (Get-Date).AddMinutes(2)
while (Get-Process -Id ([int]$env:HAARNESS_UPDATE_PID) -ErrorAction SilentlyContinue) {
  if ((Get-Date) -gt $deadline) { Report $false 'Haarness did not exit'; exit 1 }
  Start-Sleep -Milliseconds 100
}
$bin = $env:HAARNESS_UPDATE_BIN.TrimEnd('\\', '/')
$ours = @(Get-Process -ErrorAction SilentlyContinue | Where-Object {
  try { $_.Path -and ($_.Path.StartsWith($bin + '\\', [StringComparison]::OrdinalIgnoreCase) -or $_.Path.StartsWith($bin + '/', [StringComparison]::OrdinalIgnoreCase)) } catch { $false }
} | ForEach-Object { $_.Id })
if ($ours.Count -gt 0 -and (Test-Path -LiteralPath $env:HAARNESS_UPDATE_RECORDS)) {
  foreach ($folder in Get-ChildItem -LiteralPath $env:HAARNESS_UPDATE_RECORDS -Directory -ErrorAction SilentlyContinue) {
    try {
      $record = Get-Content -Raw -LiteralPath (Join-Path $folder.FullName 'runtime.json') | ConvertFrom-Json
      if (-not ($ours -contains [int]$record.pid)) { continue }
      if ($record.url -notmatch '^http://(127\\.0\\.0\\.1|localhost|\\[::1\\]|0\\.0\\.0\\.0|\\[::\\]):(\\d+)') { continue }
      $hostName = @{ '0.0.0.0' = '127.0.0.1'; '[::]' = '[::1]' }[$Matches[1]]
      if (-not $hostName) { $hostName = $Matches[1] }
      Invoke-RestMethod -Method Post -Uri ('http://' + $hostName + ':' + $Matches[2] + '/v1/shutdown') -Headers @{ Authorization = 'Bearer ' + $record.token } -TimeoutSec 5 | Out-Null
    } catch {}
  }
  $deadline = (Get-Date).AddSeconds(15)
  while ((Get-Date) -lt $deadline -and @($ours | Where-Object { Get-Process -Id $_ -ErrorAction SilentlyContinue }).Count -gt 0) {
    Start-Sleep -Milliseconds 200
  }
}
try {
  $setup = Start-Process -FilePath $env:HAARNESS_UPDATE_SETUP -ArgumentList '--updated', '/S', '--force-run' -PassThru
  $null = $setup.Handle
  $setup.WaitForExit()
  $code = $setup.ExitCode
} catch {
  $code = -1
}
if ($code -eq 0) { Report $true '' } else {
  Report $false ('the setup ended with ' + $code)
  # Whatever it left in place opens again, so no one is left without Haarness.
  if (Test-Path -LiteralPath $env:HAARNESS_UPDATE_APP) { Start-Process -FilePath $env:HAARNESS_UPDATE_APP }
}
`;

/** The Windows PowerShell command that runs `WINDOWS_HELPER` for a staged setup. */
export function windowsInstallCommand(input: {
  staged: string;
  version: string;
  result: string;
  env: NodeJS.ProcessEnv;
  pid: number;
  windows: { bin: string; records: string; app: string };
}) {
  const env: NodeJS.ProcessEnv = {
    ...input.env,
    HAARNESS_UPDATE_PID: String(input.pid),
    HAARNESS_UPDATE_SETUP: input.staged,
    HAARNESS_UPDATE_RESULT: input.result,
    HAARNESS_UPDATE_VERSION: input.version,
    HAARNESS_UPDATE_BIN: input.windows.bin,
    HAARNESS_UPDATE_RECORDS: input.windows.records,
    HAARNESS_UPDATE_APP: input.windows.app,
  };
  return {
    file: win32.join(input.env.SystemRoot ?? "C:\\Windows", "System32", "WindowsPowerShell", "v1.0", "powershell.exe"),
    args: ["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-WindowStyle", "Hidden", "-EncodedCommand", Buffer.from(WINDOWS_HELPER, "utf16le").toString("base64")],
    env,
  };
}

/** Starts installing as Haarness exits (see `HELPER`, `WINDOWS_HELPER`). */
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
  /** Windows: Desktop's `resources\bin`, the servers' records folder, and Haarness.exe. */
  windows?: { bin: string; records: string; app: string };
}) {
  if (input.platform === "win32") {
    if (!input.windows) throw new Error("The Windows install needs its folders.");
    const command = windowsInstallCommand({ ...input, pid: process.pid, windows: input.windows });
    if (existsSync(command.file)) {
      spawn(command.file, command.args, { detached: true, stdio: "ignore", windowsHide: true, env: command.env }).unref();
      return;
    }
    // A Windows without Windows PowerShell: the setup alone, which still
    // ends what runs from the folder and opens Haarness when done.
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

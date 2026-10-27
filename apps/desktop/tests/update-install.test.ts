import test from "node:test";
import assert from "node:assert/strict";
import { execFileSync, spawn, spawnSync } from "node:child_process";
import { chmod, copyFile, mkdir, mkdtemp, readdir, readFile, rm, stat, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { HELPER, WINDOWS_HELPER, detectInstall, openInstaller, removeLeftovers, stageMacApp, windowsInstallCommand } from "../src/main/update-install";

const temporary = () => mkdtemp(join(tmpdir(), "haarness-update-install-"));
/** A process id that has already exited. */
async function gone() {
  const child = spawn("/bin/sh", ["-c", "exit 0"]);
  await new Promise((resolve) => child.on("exit", resolve));
  return child.pid!;
}
function helper(args: string[]) {
  return spawnSync("/bin/sh", ["-c", HELPER, "sh", ...args], { encoding: "utf8", timeout: 30_000 });
}

test("how a copy updates follows where it runs and what it may write", async () => {
  const root = await temporary();
  const bundle = join(root, "Applications", "Haarness.app");
  await mkdir(join(bundle, "Contents", "MacOS"), { recursive: true });
  const exec = join(bundle, "Contents", "MacOS", "Haarness");
  const mac = (execPath: string, packaged = true) => detectInstall({ platform: "darwin", execPath, packaged, env: {} });
  assert.deepEqual(await mac(exec), { method: "restart", kind: "zip", target: bundle });
  assert.deepEqual(await mac(exec, false), { method: "off" });
  assert.deepEqual(await mac("/private/var/folders/x/AppTranslocation/ABC/d/Haarness.app/Contents/MacOS/Haarness"), { method: "move", kind: "zip" });
  assert.deepEqual(await mac("/Volumes/Haarness 0.1.5/Haarness.app/Contents/MacOS/Haarness"), { method: "move", kind: "zip" });
  assert.deepEqual(await mac("/usr/local/bin/haarness-desktop"), { method: "off" });
  if (process.getuid?.() !== 0) {
    await chmod(join(root, "Applications"), 0o555);
    assert.deepEqual(await mac(exec), { method: "installer", kind: "dmg" });
    await chmod(join(root, "Applications"), 0o755);
  }
  // A test names the app it stands in for.
  assert.deepEqual(await detectInstall({ platform: "darwin", execPath: "/x/Electron", packaged: false, env: {}, testTarget: bundle }), {
    method: "restart",
    kind: "zip",
    target: bundle,
  });
  const image = join(root, "Haarness.AppImage");
  await writeFile(image, "");
  const linux = (env: NodeJS.ProcessEnv, execPath = "/tmp/.mount_x/haarness-desktop") =>
    detectInstall({ platform: "linux", execPath, packaged: true, env });
  assert.deepEqual(await linux({ APPIMAGE: image }), { method: "restart", kind: "AppImage", target: image });
  assert.deepEqual(await linux({}, "/opt/Haarness/haarness-desktop"), { method: "installer", kind: "deb" });
  assert.deepEqual(await linux({ APPIMAGE: "/nonexistent/Haarness.AppImage" }), { method: "installer", kind: "AppImage" });
  assert.deepEqual(await detectInstall({ platform: "win32", execPath: "C:\\Users\\a\\AppData\\Local\\Programs\\Haarness\\Haarness.exe", packaged: true, env: {} }), {
    method: "restart",
    kind: "exe",
    target: "C:\\Users\\a\\AppData\\Local\\Programs\\Haarness",
  });
  await rm(root, { recursive: true, force: true });
});

test("the install helper swaps the app once Haarness exits, and reopens it", { skip: process.platform === "win32" }, async () => {
  const root = await temporary();
  const target = join(root, "Haarness.app");
  const staged = join(root, "updates", "9.9.9", "Haarness.app");
  await mkdir(target, { recursive: true });
  await mkdir(staged, { recursive: true });
  await writeFile(join(target, "version"), "0.1.5");
  await writeFile(join(staged, "version"), "9.9.9");
  const result = join(root, "result.json");
  const opened = join(root, "opened");
  const run = helper([String(await gone()), target, staged, result, "9.9.9", "1", "/bin/sh", "-c", `echo reopened > '${opened}'`]);
  assert.equal(run.status, 0, run.stderr);
  assert.equal(await readFile(join(target, "version"), "utf8"), "9.9.9");
  assert.deepEqual(JSON.parse(await readFile(result, "utf8")), { version: "9.9.9", ok: true, reason: "" });
  // Nothing left beside it, and the staged copy moved rather than copied.
  assert.deepEqual((await readdir(root)).sort(), ["Haarness.app", "result.json", "updates"].concat(await stat(opened).then(() => ["opened"], () => [])).sort());
  assert.equal(await stat(staged).catch(() => null), null);
  for (let index = 0; index < 50 && !(await stat(opened).catch(() => null)); index++) await new Promise((resolve) => setTimeout(resolve, 50));
  assert.equal((await readFile(opened, "utf8")).trim(), "reopened");
  await rm(root, { recursive: true, force: true });
});

test("a failed install keeps the running app and the staged one, says why, and does not reopen unasked", { skip: process.platform === "win32" }, async () => {
  const root = await temporary();
  const target = join(root, "Haarness.app");
  await mkdir(target, { recursive: true });
  await writeFile(join(target, "version"), "0.1.5");
  const result = join(root, "result.json");
  // The staged app is gone (cleaned up by hand, say).
  const missing = helper([String(await gone()), target, join(root, "nothing.app"), result, "9.9.9", "0", "/usr/bin/false"]);
  assert.equal(missing.status, 0);
  assert.equal(await readFile(join(target, "version"), "utf8"), "0.1.5");
  const outcome = JSON.parse(await readFile(result, "utf8"));
  assert.equal(outcome.ok, false);
  assert.match(outcome.reason, /new version/);
  assert.deepEqual((await readdir(root)).sort(), ["Haarness.app", "result.json"]);
  // A folder Haarness may not write: nothing moves.
  if (process.getuid?.() !== 0) {
    const locked = join(root, "locked");
    const app = join(locked, "Haarness.app");
    const staged = join(root, "staged.app");
    await mkdir(app, { recursive: true });
    await mkdir(staged, { recursive: true });
    await chmod(locked, 0o555);
    helper([String(await gone()), app, staged, result, "9.9.9", "0"]);
    await chmod(locked, 0o755);
    assert.equal(JSON.parse(await readFile(result, "utf8")).ok, false);
    assert.ok(await stat(app));
    assert.ok(await stat(staged));
    assert.deepEqual(await readdir(locked), ["Haarness.app"]);
  }
  await rm(root, { recursive: true, force: true });
});

test("the install helper waits for Haarness to exit first", { skip: process.platform === "win32" }, async () => {
  const root = await temporary();
  const target = join(root, "Haarness.AppImage");
  const staged = join(root, ".Haarness.AppImage.haarness-update");
  await writeFile(target, "old");
  await writeFile(staged, "new");
  // Not a child of this process, which could not reap it while the helper runs.
  const running = execFileSync("/bin/sh", ["-c", "sleep 0.6 >/dev/null 2>&1 & echo $!"], { encoding: "utf8" }).trim();
  const started = Date.now();
  const result = join(root, "result.json");
  const run = helper([running, target, staged, result, "9.9.9", "0"]);
  assert.equal(run.status, 0);
  assert.ok(Date.now() - started >= 500);
  assert.equal(await readFile(target, "utf8"), "new");
  await rm(root, { recursive: true, force: true });
});

test("an installer goes to Downloads beside older copies, then opens", async () => {
  const root = await temporary();
  const file = join(root, "Haarness-9.9.9-linux-x64.deb");
  await writeFile(file, "deb");
  const downloads = join(root, "Downloads");
  const opened: string[] = [];
  await openInstaller(file, downloads, async (path) => (opened.push(path), ""));
  await openInstaller(file, downloads, async (path) => (opened.push(path), ""));
  assert.deepEqual(opened, [join(downloads, "Haarness-9.9.9-linux-x64.deb"), join(downloads, "Haarness-9.9.9-linux-x64 (1).deb")]);
  await assert.rejects(openInstaller(file, downloads, async () => "No application knows how to open it"), /No application/);
  await rm(root, { recursive: true, force: true });
});

test("a macOS update is unpacked and checked before anything uses it", { skip: process.platform !== "darwin" }, async () => {
  const root = await temporary();
  async function app(version: string) {
    const bundle = join(root, "build", version, "Haarness.app");
    await mkdir(join(bundle, "Contents", "MacOS"), { recursive: true });
    await copyFile("/usr/bin/true", join(bundle, "Contents", "MacOS", "Haarness"));
    await writeFile(
      join(bundle, "Contents", "Info.plist"),
      `<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
<key>CFBundleExecutable</key><string>Haarness</string>
<key>CFBundleIdentifier</key><string>dev.haarness.test</string>
<key>CFBundleShortVersionString</key><string>${version}</string>
</dict></plist>`,
    );
    execFileSync("/usr/bin/codesign", ["--force", "--deep", "--sign", "-", bundle]);
    return bundle;
  }
  const zip = (bundle: string, name: string) => {
    const file = join(root, name);
    execFileSync("/usr/bin/ditto", ["-c", "-k", "--keepParent", bundle, file]);
    return file;
  };
  const folder = join(root, "updates", "9.9.9");
  await mkdir(folder, { recursive: true });
  const good = await app("9.9.9");
  const staged = await stageMacApp({ file: zip(good, "good.zip"), version: "9.9.9", folder });
  assert.equal(staged, join(folder, "Haarness.app"));
  assert.deepEqual(await readdir(folder), ["Haarness.app"]);
  // Another version than the release names.
  await assert.rejects(stageMacApp({ file: zip(await app("9.9.8"), "old.zip"), version: "9.9.9", folder }), (error: { code?: string }) => error.code === "damaged");
  // Changed after it was signed.
  const tampered = await app("9.9.7");
  await writeFile(join(tampered, "Contents", "MacOS", "Haarness"), "#!/bin/sh\necho owned\n");
  await assert.rejects(stageMacApp({ file: zip(tampered, "tampered.zip"), version: "9.9.7", folder }), (error: { code?: string }) => error.code === "signature");
  // Signed, but not by the team that signed the running app.
  await assert.rejects(
    stageMacApp({ file: zip(await app("9.9.6"), "adhoc.zip"), version: "9.9.6", folder, team: "J63GND4Q8A" }),
    (error: { code?: string }) => error.code === "signature",
  );
  // A running app whose own signature cannot be read accepts nothing.
  await assert.rejects(
    stageMacApp({ file: zip(await app("9.9.5"), "unreadable.zip"), version: "9.9.5", folder, team: null }),
    (error: { code?: string }) => error.code === "signature",
  );
  // Not an archive at all.
  const junk = join(root, "junk.zip");
  await writeFile(junk, "not a zip");
  await assert.rejects(stageMacApp({ file: junk, version: "9.9.9", folder }), (error: { code?: string }) => error.code === "write");
  await rm(root, { recursive: true, force: true });
});

test("what a stopped install left beside the app goes once it is old; a running install's is kept", async () => {
  const root = await temporary();
  const target = join(root, "Haarness.app");
  await mkdir(target);
  for (const name of [".Haarness.app.previous.123", ".Haarness.app.incoming.456", ".Haarness.app.previous.789", ".Other.app.previous.1", "Haarness.app.previous.2"])
    await mkdir(join(root, name));
  const old = Date.now() - 11 * 60_000;
  const { utimes } = await import("node:fs/promises");
  for (const name of [".Haarness.app.previous.123", ".Haarness.app.incoming.456", ".Other.app.previous.1", "Haarness.app.previous.2"])
    await utimes(join(root, name), old / 1000, old / 1000);
  await removeLeftovers(target);
  assert.deepEqual((await readdir(root)).sort(), [".Haarness.app.previous.789", ".Other.app.previous.1", "Haarness.app", "Haarness.app.previous.2"]);
  await rm(root, { recursive: true, force: true });
});

test("Windows runs its install helper through Windows PowerShell, with every input in the environment", () => {
  const command = windowsInstallCommand({
    staged: "C:\\Users\\a b\\AppData\\Roaming\\Haarness\\updates\\9.9.9\\Haarness-9.9.9-win-x64.exe",
    version: "9.9.9",
    result: "C:\\Users\\a b\\AppData\\Roaming\\Haarness\\updates\\install-result.json",
    env: { SystemRoot: "D:\\Windows", PATH: "x" },
    pid: 4242,
    windows: { bin: "C:\\Programs\\Haarness\\resources\\bin", records: "C:\\Users\\a b\\.cache\\haarness\\workspaces", app: "C:\\Programs\\Haarness\\Haarness.exe" },
  });
  assert.equal(command.file, "D:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe");
  const encoded = command.args.at(-1)!;
  assert.deepEqual(command.args.slice(0, -1), ["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-WindowStyle", "Hidden", "-EncodedCommand"]);
  assert.equal(Buffer.from(encoded, "base64").toString("utf16le"), WINDOWS_HELPER);
  // Windows caps a command line at 32,767 characters.
  assert.ok(command.args.join(" ").length < 20_000);
  assert.equal(command.env.HAARNESS_UPDATE_PID, "4242");
  assert.equal(command.env.HAARNESS_UPDATE_VERSION, "9.9.9");
  assert.equal(command.env.HAARNESS_UPDATE_BIN, "C:\\Programs\\Haarness\\resources\\bin");
  assert.equal(command.env.PATH, "x");
  // The helper names the flags electron-updater uses and the graceful stop it asks for first.
  assert.match(WINDOWS_HELPER, /'--updated', '\/S', '--force-run'/);
  assert.match(WINDOWS_HELPER, /\/v1\/shutdown/);
  assert.match(WINDOWS_HELPER, /StartsWith\(\$bin \+ '\\'/);
});

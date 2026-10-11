import { spawnSync } from "node:child_process";
import { mkdir, copyFile, chmod, access } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { resolve, join, delimiter } from "node:path";
const directory = fileURLToPath(new URL("..", import.meta.url));
const root = resolve(directory, "../..");
function run(command, args, cwd = root) {
  const result = spawnSync(command, args, {
    cwd,
    stdio: "inherit",
    shell: process.platform === "win32",
  });
  if (result.status !== 0) process.exit(result.status || 1);
}
const npm = process.platform === "win32" ? "npm.cmd" : "npm";
const name = process.platform === "win32" ? "haarness.exe" : "haarness";
let binary = process.env.HAARNESS_BIN;
// A development build signs in to the local Cloud stack, never production;
// shipping one would give users an app that cannot sign in.
if (binary && /[\\/]target[\\/]debug[\\/]/.test(resolve(binary)) && process.env.HAARNESS_PACKAGE_DEBUG !== "1") {
  console.error(
    `${binary} is a development build: it signs in to the local Cloud stack, not app.haarness.ai.\n` +
      "Package a release build (unset HAARNESS_BIN), or set HAARNESS_PACKAGE_DEBUG=1 for a local-only package.",
  );
  process.exit(1);
}
if (!binary) {
  run("cargo", ["build", "--release", "-p", "haarness-cli"]);
  binary = join(root, "target/release", name);
}
await access(binary);
run(npm, ["run", "build"], join(root, "sdks/typescript"));
// Haarness for Chrome ships inside the app (resources/browser-extension/chrome),
// where `haarness browser setup` finds it next to the bundled binary.
// It links the shared browser driver, whose own dependencies must be installed first.
const extension = join(root, "apps/browser-extension");
for (const directory of [join(root, "packages/browser-driver"), extension]) {
  try {
    await access(join(directory, "node_modules"));
  } catch {
    run(npm, ["ci"], directory);
  }
}
run(npm, ["run", "build"], extension);
run(npm, ["run", "build"], directory);
await mkdir(join(directory, "resources"), { recursive: true });
await copyFile(resolve(binary), join(directory, "resources", name));
await chmod(join(directory, "resources", name), 0o755);

// Bundle the JavaScript runtime used by extensions and workflows so installations
// launched from Finder or the Start menu do not depend on a developer shell.
const bunName = process.platform === "win32" ? "bun.exe" : "bun";
const bunCandidates = [
  process.env.HAARNESS_BUN,
  ...(process.env.PATH ?? "")
    .split(delimiter)
    .map((path) => join(path, bunName)),
].filter(Boolean);
let bun;
for (const candidate of bunCandidates) {
  try {
    await access(candidate);
    bun = candidate;
    break;
  } catch {}
}
if (!bun)
  throw new Error(
    "Bun 1.3.14 is required to package extensions and workflows.",
  );
const version = spawnSync(bun, ["--version"], { encoding: "utf8" });
if (version.status !== 0 || version.stdout.trim() !== "1.3.14")
  throw new Error(
    "Package with Bun 1.3.14 to match the tested runtime and bundled license.",
  );
await copyFile(bun, join(directory, "resources", bunName));
await chmod(join(directory, "resources", bunName), 0o755);
await copyFile(
  join(directory, "assets/BUN-LICENSE.md"),
  join(directory, "resources/BUN-LICENSE.md"),
);

// The backend embeds ripgrep; ship its notices beside the other bundled tools.
for (const name of ["COPYING", "LICENSE-MIT", "UNLICENSE"])
  await copyFile(
    join(root, "vendor/ripgrep", name),
    join(directory, "resources", `RIPGREP-${name}.txt`),
  );

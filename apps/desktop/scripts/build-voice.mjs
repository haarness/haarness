import { spawnSync } from "node:child_process";
import { mkdir, rm, stat } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { join } from "node:path";
if (process.platform === "darwin") {
  const root = fileURLToPath(new URL("..", import.meta.url));
  const source = join(root, "native/voice.swift");
  const info = join(root, "native/voice-info.plist");
  const output = join(root, "resources/haarness-voice");
  // One universal helper for both Mac packages: a Mac packages the other
  // architecture's app from this same resources folder, and a helper built
  // only for the packaging Mac (0.1.3's x64 app carried an arm64 one) cannot
  // start on the other kind, so dictation fails there.
  const architectures = ["arm64", "x86_64"];
  const built = await stat(output).catch(() => ({ mtimeMs: 0 }));
  const slices = spawnSync("xcrun", ["lipo", "-archs", output], {
    encoding: "utf8",
  }).stdout?.split(/\s+/) ?? [];
  if (
    built.mtimeMs <
      Math.max((await stat(source)).mtimeMs, (await stat(info)).mtimeMs) ||
    !architectures.every((architecture) => slices.includes(architecture))
  ) {
    await mkdir(join(root, "resources"), { recursive: true });
    const run = (args) => {
      const result = spawnSync("xcrun", args, { stdio: "inherit" });
      if (result.status !== 0) process.exit(result.status || 1);
    };
    for (const architecture of architectures)
      run([
        "swiftc",
        "-O",
        "-swift-version",
        "5",
        "-target",
        `${architecture}-apple-macosx14.0`,
        source,
        "-o",
        `${output}-${architecture}`,
        "-framework",
        "AVFoundation",
        "-framework",
        "Speech",
        "-Xlinker",
        "-sectcreate",
        "-Xlinker",
        "__TEXT",
        "-Xlinker",
        "__info_plist",
        "-Xlinker",
        info,
      ]);
    run([
      "lipo",
      "-create",
      "-output",
      output,
      ...architectures.map((architecture) => `${output}-${architecture}`),
    ]);
    for (const architecture of architectures)
      await rm(`${output}-${architecture}`, { force: true });
  }
}

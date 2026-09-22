import { spawnSync } from "node:child_process";
import { mkdir, stat } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { join } from "node:path";
if (process.platform === "darwin") {
  const root = fileURLToPath(new URL("..", import.meta.url));
  const source = join(root, "native/voice.swift");
  const info = join(root, "native/voice-info.plist");
  const output = join(root, "resources/haarness-voice");
  const built = await stat(output).catch(() => ({ mtimeMs: 0 }));
  if (
    built.mtimeMs <
    Math.max((await stat(source)).mtimeMs, (await stat(info)).mtimeMs)
  ) {
    await mkdir(join(root, "resources"), { recursive: true });
    const result = spawnSync(
      "xcrun",
      [
        "swiftc",
        "-O",
        "-swift-version",
        "5",
        "-target",
        `${process.arch === "arm64" ? "arm64" : "x86_64"}-apple-macosx14.0`,
        source,
        "-o",
        output,
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
      ],
      { stdio: "inherit" },
    );
    if (result.status !== 0) process.exit(result.status || 1);
  }
}

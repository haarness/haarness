import { execFile } from "node:child_process";
import { stat } from "node:fs/promises";
import { homedir } from "node:os";
import { nativeImage } from "electron";
import { parseDefaultsLocation, screenshotFolder } from "../screenshots";
import { createScreenshotThumbnails } from "./screenshot-thumbnails";

/** The Screenshot app's "Save to" folder, when it names a folder that exists. */
async function macLocation(home: string) {
  if (process.platform !== "darwin") return undefined;
  const stdout = await new Promise<string>((resolve) =>
    execFile(
      "/usr/bin/defaults",
      ["read", "com.apple.screencapture", "location"],
      { timeout: 3000 },
      (error, output) => resolve(error ? "" : String(output)),
    ),
  );
  const location = parseDefaultsLocation(stdout, home);
  if (!location) return undefined;
  return (await stat(location).then((info) => info.isDirectory(), () => false)) ? location : undefined;
}

/** The folder the picker opens, or none on Linux until one is chosen. */
export async function defaultScreenshotFolder(remembered: unknown) {
  // End-to-end runs point the picker at a fixture folder, never the person's own.
  if (process.env.HAARNESS_DESKTOP_TEST === "1" && process.env.HAARNESS_SCREENSHOTS_TEST_DIR)
    return process.env.HAARNESS_SCREENSHOTS_TEST_DIR;
  const home = homedir();
  return screenshotFolder({
    platform: process.platform,
    home,
    location: await macLocation(home),
    remembered,
  });
}

/** Thumbnails for listed screenshots, kept while their files stay the same. */
export const screenshotThumbnails = createScreenshotThumbnails({ platform: process.platform, images: nativeImage });

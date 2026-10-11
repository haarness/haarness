import { app, shell } from "electron";
import { spawn } from "node:child_process";
import { realpath, stat } from "node:fs/promises";
import { isAbsolute, join } from "node:path";
import { z } from "zod";
import type { ChromeSetupInfo } from "../chrome";
import { environment, executable } from "./runtime";

/**
 * `haarness browser setup|status --json`, run with the same binary Desktop
 * starts its projects with. Setup registers the native host that Chrome
 * starts and copies the unpacked extension; status only reads.
 */
const report = z
  .object({
    hostName: z.string(),
    extensionId: z.string(),
    webStoreUrl: z.string().nullable().optional(),
    executable: z.string().optional(),
    wrapper: z.string().optional(),
    removed: z.boolean().optional(),
    warnings: z.array(z.string()).optional(),
    browsers: z.array(
      z
        .object({
          browser: z.string(),
          name: z.string(),
          extensionsPage: z.string().optional(),
          manifest: z.string().nullable().optional(),
          detected: z.boolean().optional(),
          registered: z.boolean().optional(),
          supported: z.boolean().optional(),
          note: z.string().nullable().optional(),
        })
        .passthrough(),
    ),
    extension: z
      .object({ path: z.string(), version: z.string().optional() })
      .passthrough()
      .nullable()
      .optional(),
  })
  .passthrough();
/** The unpacked extension folder the last report named. Reveal shows only
 * this folder, never a path the renderer supplies. */
let extensionFolder: string | undefined;

function run(args: string[]) {
  return executable().then(
    (binary) =>
      new Promise<string>((resolve, reject) => {
        const child = spawn(binary, ["browser", ...args], {
          cwd: app.getPath("home"),
          env: environment(),
          stdio: ["ignore", "pipe", "pipe"],
          windowsHide: true,
        });
        let output = "",
          errors = "";
        const timer = setTimeout(() => child.kill(), 60_000);
        child.stdout.on("data", (chunk) => {
          output += chunk.toString();
          if (output.length > 1024 * 1024) child.kill();
        });
        child.stderr.on("data", (chunk) => {
          errors = (errors + chunk.toString()).slice(-4000);
        });
        child.once("error", (error) => {
          clearTimeout(timer);
          reject(error);
        });
        child.once("close", (code) => {
          clearTimeout(timer);
          if (code === 0) resolve(output);
          else
            reject(
              new Error(
                errors.trim() || "Haarness could not set up Chrome. Please retry.",
              ),
            );
        });
      }),
  );
}

export async function chromeSetup(
  action: "setup" | "status",
  options: string[] = [],
): Promise<ChromeSetupInfo> {
  // A test run registers only with a test profile's folder
  // (HAARNESS_BROWSER_MANIFEST_DIR), never with the user's own Chrome.
  if (
    action === "setup" &&
    process.env.HAARNESS_DESKTOP_TEST &&
    !process.env.HAARNESS_BROWSER_MANIFEST_DIR
  )
    throw new Error(
      "Test runs set HAARNESS_BROWSER_MANIFEST_DIR before setting up Chrome.",
    );
  const output = await run([action, "--json", ...options]);
  let parsed: unknown;
  try {
    parsed = JSON.parse(output);
  } catch {
    throw new Error("Haarness returned an unreadable Chrome setup report.");
  }
  const value = report.parse(parsed) as ChromeSetupInfo;
  const path = value.extension?.path;
  extensionFolder = path && isAbsolute(path) ? path : undefined;
  return value;
}

/** Show the unpacked extension folder in Finder, Explorer or Files. */
export async function revealChromeExtension() {
  if (!extensionFolder) await chromeSetup("status");
  if (!extensionFolder)
    throw new Error("Set up Chrome first; there is no extension folder yet.");
  const folder = await realpath(extensionFolder);
  if (
    !(await stat(folder)).isDirectory() ||
    !(await stat(join(folder, "manifest.json")).catch(() => undefined))
  )
    throw new Error("The Haarness for Chrome folder is missing. Set up Chrome again.");
  shell.showItemInFolder(folder);
}

/**
 * An installed Desktop keeps Chrome's registration pointed at itself after
 * an update or a move, once per launch and in the background. Isolated and
 * test runs never touch the user's real Chrome registration.
 */
export function setupChromeOnLaunch() {
  if (
    !app.isPackaged ||
    process.env.HAARNESS_DESKTOP_TEST ||
    process.env.HAARNESS_DATA_DIR ||
    process.env.HAARNESS_DESKTOP_DATA
  )
    return;
  void chromeSetup("setup", ["--quiet", "--if-missing"]).catch(() => {});
}

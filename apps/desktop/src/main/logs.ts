import { app, dialog, shell, type BrowserWindow } from "electron";
import { spawn } from "node:child_process";
import { join } from "node:path";
import { z } from "zod";
import { DETAILED_LOGGING, logsFileName, type ExportedLogs, type LogLevelStatus } from "../diagnostics";
import { environment, executable } from "./runtime";

// This computer's Haarness logs, through `haarness logs … --json`, run with
// the binary and data folder Desktop starts its projects with: the same
// command the CLI and the TUI's /logs use, so the three agree.
const status = z.object({
  level: z.string().nullable(),
  until: z.string().nullable(),
  untilStopped: z.boolean(),
});
const exported = z.object({
  path: z.string(),
  files: z.number(),
  skipped: z.number(),
});

function run(args: string[]) {
  return executable().then(
    (binary) =>
      new Promise<unknown>((resolve, reject) => {
        const child = spawn(binary, ["logs", ...args, "--json"], {
          cwd: app.getPath("home"),
          env: environment(),
          stdio: ["ignore", "pipe", "pipe"],
          windowsHide: true,
        });
        let output = "",
          errors = "";
        // An export reads up to 200 MB of logs.
        const timer = setTimeout(() => child.kill(), 180_000);
        child.stdout.on("data", (chunk) => {
          output += chunk.toString();
          if (output.length > 64 * 1024) child.kill();
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
          if (code !== 0) {
            const last = errors.trim().split("\n").at(-1)?.replace(/^Error:\s*/, "");
            return reject(new Error(last || "Haarness could not read its logs. Please retry."));
          }
          try {
            resolve(JSON.parse(output));
          } catch {
            reject(new Error("Haarness returned an unreadable logs report."));
          }
        });
      }),
  );
}

/** The raised level now, or Detailed logging turned on (debug for 30 minutes) or off. */
export async function logLevel(action: "status" | "detail-on" | "detail-off"): Promise<LogLevelStatus> {
  const args =
    action === "detail-on"
      ? ["level", "debug", "--for", DETAILED_LOGGING]
      : action === "detail-off"
        ? ["level", "stop"]
        : ["level"];
  return status.parse(await run(args));
}

/**
 * Asks where to save the logs (Downloads, named for today), packs them with
 * known secret shapes removed, and shows the zip in Finder, Explorer or the
 * file manager. Null when the person cancelled.
 */
export async function exportLogs(
  win: BrowserWindow,
  say: (key: string) => string,
): Promise<ExportedLogs | null> {
  const choice = await dialog.showSaveDialog(win, {
    title: say("Export logs"),
    buttonLabel: say("Export"),
    defaultPath: join(app.getPath("downloads"), logsFileName(new Date())),
    filters: [{ name: say("Zip archive"), extensions: ["zip"] }],
  });
  if (choice.canceled || !choice.filePath) return null;
  const value = exported.parse(await run(["export", "--output", choice.filePath]));
  shell.showItemInFolder(value.path);
  return value;
}

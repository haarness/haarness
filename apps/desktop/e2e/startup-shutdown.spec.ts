import { test, expect } from "@playwright/test";
import { spawn } from "node:child_process";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import electron from "electron";
import { isolatedEnv } from "./fixtures/isolated-env";

for (const outcome of ["resolve", "reject"] as const) {
  test(`quit during startup exits cleanly when preferences ${outcome} during native teardown`, async () => {
    const directory = await mkdtemp(
      join(tmpdir(), "haarness-startup-shutdown-"),
    );
    const entry = join(directory, "entry.cjs");
    await writeFile(
      entry,
      `
      const { app, webContents } = require("electron");
      const fs = require("node:fs");
      const { syncBuiltinESMExports } = require("node:module");
      const { basename } = require("node:path");
      const original = fs.promises.readFile;
      let sentinel;
      app.whenReady().then(() => { sentinel = webContents.create({}); });
      fs.promises.readFile = function(path, ...args) {
        if (basename(String(path)) !== "desktop.json")
          return original.call(this, path, ...args);
        return new Promise((resolve, reject) => {
          // Unowned web contents survive until native teardown. Completing the
          // pending startup read from their destruction reproduces the reported
          // RenderProcessHostImpl CHECK on the unguarded app.
          sentinel.once("destroyed", () => {
            process.stdout.write("startup-read-completed-during-teardown\\n");
            ${outcome === "resolve" ? 'resolve("{}");' : 'reject(new Error("Test preference read failed"));'}
          });
          app.quit();
        });
      };
      syncBuiltinESMExports();
      import(${JSON.stringify(pathToFileURL(resolve("out/main/index.js")).href)});
    `,
    );
    const env = isolatedEnv(["ELECTRON_RUN_AS_NODE", "ELECTRON_RENDERER_URL"]);
    Object.assign(env, {
      HAARNESS_DESKTOP_DATA: join(directory, "desktop"),
      HAARNESS_DATA_DIR: join(directory, "data"),
      HAARNESS_DESKTOP_TEST: "1",
      ELECTRON_RENDERER_URL: "data:text/html,Startup-test",
    });
    // As Playwright launches Electron on Linux: a CI runner's chrome-sandbox
    // is not set up to run as root.
    const flags = process.platform === "linux" ? ["--no-sandbox"] : [];
    const child = spawn(electron as unknown as string, [...flags, entry], { env });
    let output = "";
    child.stdout.on("data", (data) => {
      output += data;
    });
    child.stderr.on("data", (data) => {
      output += data;
    });
    let timer: ReturnType<typeof setTimeout> | undefined;
    try {
      const result = await new Promise<{
        code: number | null;
        signal: string | null;
      }>((resolve, reject) => {
        child.once("error", reject);
        child.once("exit", (code, signal) => resolve({ code, signal }));
        timer = setTimeout(() => {
          child.kill("SIGKILL");
          reject(new Error(`Desktop did not quit: ${output}`));
        }, 30000);
      });
      // Electron destroys unowned web contents before exiting only on macOS,
      // where the reported crash was; elsewhere the read stays pending and the
      // app must still exit cleanly.
      if (process.platform === "darwin")
        expect(output).toContain("startup-read-completed-during-teardown");
      expect(result, output).toEqual({ code: 0, signal: null });
    } finally {
      clearTimeout(timer);
      if (child.exitCode === null && child.signalCode === null)
        child.kill("SIGKILL");
      await rm(directory, { recursive: true, force: true });
    }
  });
}

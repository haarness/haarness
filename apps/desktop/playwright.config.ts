import { defineConfig } from "@playwright/test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
// Test terminals start the developer's own shell. Keep oh-my-zsh's update
// question from taking the first keystrokes a test types into it.
process.env.DISABLE_AUTO_UPDATE ??= "true";
// The servers the specs start never report usage statistics (usage-statistics.spec.ts turns them on).
process.env.HAARNESS_TELEMETRY ??= "0";
// Nor do they hand a schedule to launchd, systemd or Task Scheduler, which an
// isolated data folder does not move; a spec that runs one natively registers
// it itself.
process.env.HAARNESS_SCHEDULE_SKIP_INSTALL ??= "1";
// The specs' chats keep their own temp folders (their commands' TMPDIR) in a
// folder of the run's own, short like the /tmp/haarness-<uid> a person's
// Haarness uses, and removed when the run ends, so a run leaves none in that
// one. Only a debug build or the test providers read the variable; the
// workers inherit the runner's folder.
if (!process.env.HAARNESS_TEST_CHAT_TEMP) {
  const folder = mkdtempSync(join(process.platform === "win32" ? tmpdir() : "/tmp", "hct-"));
  process.env.HAARNESS_TEST_CHAT_TEMP = folder;
  process.on("exit", () => rmSync(folder, { recursive: true, force: true }));
}
export default defineConfig({
  testDir: "./e2e",
  fullyParallel: false,
  workers: 1,
  timeout: 120_000,
  expect: { timeout: 15_000 },
  // A shared CI runner can stall any one of these several hundred window
  // tests; a test that passes only on a second try is still reported flaky.
  retries: process.env.CI ? 2 : 0,
  reporter: [["list"], ["html", { open: "never" }]],
  use: { trace: "retain-on-failure", screenshot: "only-on-failure" },
});

import { defineConfig } from "@playwright/test";
// Test terminals start the developer's own shell. Keep oh-my-zsh's update
// question from taking the first keystrokes a test types into it.
process.env.DISABLE_AUTO_UPDATE ??= "true";
// The servers the specs start never report usage statistics (usage-statistics.spec.ts turns them on).
process.env.HAARNESS_TELEMETRY ??= "0";
// Nor do they hand a schedule to launchd, systemd or Task Scheduler, which an
// isolated data folder does not move; a spec that runs one natively registers
// it itself.
process.env.HAARNESS_SCHEDULE_SKIP_INSTALL ??= "1";
export default defineConfig({
  testDir: "./e2e",
  fullyParallel: false,
  workers: 1,
  timeout: 120_000,
  expect: { timeout: 15_000 },
  retries: 0,
  reporter: [["list"], ["html", { open: "never" }]],
  use: { trace: "retain-on-failure", screenshot: "only-on-failure" },
});

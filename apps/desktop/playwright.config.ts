import { defineConfig } from "@playwright/test";
// Test terminals start the developer's own shell. Keep oh-my-zsh's update
// question from taking the first keystrokes a test types into it.
process.env.DISABLE_AUTO_UPDATE ??= "true";
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

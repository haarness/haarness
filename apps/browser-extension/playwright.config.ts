import { defineConfig } from "@playwright/test";

// Each test starts its own Chromium with the built extension. The fake-bridge
// project plays the bridge and servers itself (e2e/fixtures/extension.ts); the
// full-stack project runs a real haarness daemon and bridge (e2e/fixtures/stack.ts)
// and needs a built haarness (HAARNESS_BIN, or target/debug|release).
export default defineConfig({
  testDir: "./e2e",
  fullyParallel: false,
  workers: 1,
  timeout: 90_000,
  expect: { timeout: 15_000 },
  retries: 0,
  reporter: [["list"], ["html", { open: "never" }]],
  use: { trace: "retain-on-failure", screenshot: "only-on-failure" },
  projects: [
    { name: "fake-bridge", testMatch: ["bridge.spec.ts", "ui.spec.ts"] },
    { name: "full-stack", testMatch: ["stack*.spec.ts"], timeout: 180_000 },
  ],
});

/**
 * The popup and welcome page in a real Chromium, in light and dark, with
 * screenshots under test-results/screens for review.
 */
import type { Page } from "@playwright/test";
import { join } from "node:path";
import { askPolicy, SERVER, THREAD, type FakeBridge } from "./fixtures/bridge";
import { expect, test, type Site } from "./fixtures/extension";

const SECOND = "0192a5f0-7c1e-7d3a-9b1e-4f2a9c3d5e63";

async function busy(bridge: FakeBridge, site: Site) {
  await bridge.wait((message) => message.type === "hello");
  bridge.start({ extension: { path: "/Users/you/Library/Application Support/haarness/browser/chrome/Haarness for Chrome", version: "0.1.1-preview.1" } });
  bridge.attach();
  for (const path of ["/", "/pricing"]) {
    const url = site.origin + path;
    await bridge.command({ action: { type: "tabs.open", url }, policy: askPolicy, target: { resources: [url], key: "open" } });
  }
  const url = `${site.origin}/careers`;
  await bridge.command(
    { action: { type: "tabs.open", url }, policy: askPolicy, target: { resources: [url], key: "open" } },
    { thread: SECOND, title: "Compare hosting plans for the new marketing site" },
  );
  bridge.send({
    type: "activity",
    server: SERVER,
    active: [THREAD],
    approvals: [
      {
        id: "0192a5f0-0000-7000-8000-000000000001",
        thread: THREAD,
        patterns: [`chrome-browsing:${THREAD}:https://www.figma.com`, `chrome-scripts:${THREAD}:https://www.figma.com`],
        explicit: true,
      },
    ],
  });
}

async function shoot(page: Page, name: string) {
  await page.evaluate(() => document.fonts.ready);
  const path = join(test.info().project.outputDir, "screens", `${name}.png`);
  await page.screenshot({ path, fullPage: true, animations: "disabled" });
  await test.info().attach(name, { path, contentType: "image/png" });
}

for (const dark of [false, true]) {
  const theme = dark ? "dark" : "light";

  test(`popup with chats and an approval (${theme})`, async ({ bridge, site, openExtensionPage }) => {
    await busy(bridge, site);
    const popup = await openExtensionPage("popup.html", { dark });
    await expect(popup.getByRole("status").first()).toHaveText("Connected");
    await expect(popup.getByText("Use figma.com", { exact: false })).toHaveCount(0);
    await expect(popup.getByText("Waiting for you", { exact: true })).toBeVisible();
    await expect(popup.getByRole("button", { name: "Allow in this chat" })).toBeVisible();
    await expect(popup.getByText("Research trip").first()).toBeVisible();
    await expect(popup.getByText("2 tabs")).toBeVisible();
    await expect(popup.getByRole("button", { name: "Show tabs" })).toHaveCount(2);
    await expect(popup.getByRole("button", { name: "Stop", exact: true })).toHaveCount(1);
    await expect(popup.getByRole("switch", { name: "Let Haarness use this browser" })).toBeChecked();
    // Keyboard users reach the approval first.
    await popup.keyboard.press("Tab");
    await expect(popup.getByRole("button", { name: "Allow in this chat" })).toBeFocused();
    await shoot(popup, `popup-busy-${theme}`);
  });

  test(`popup with an approval only Haarness can answer (${theme})`, async ({ bridge, site, openExtensionPage }) => {
    await busy(bridge, site);
    // An upload of a file outside the project: the Chrome site and the file wait in one prompt.
    bridge.send({
      type: "activity",
      server: SERVER,
      active: [THREAD],
      approvals: [
        {
          id: "0192a5f0-0000-7000-8000-000000000002",
          thread: THREAD,
          patterns: [`chrome-uploads:${THREAD}:https://www.figma.com`],
          explicit: true,
          answerable: false,
        },
      ],
    });
    const popup = await openExtensionPage("popup.html", { dark });
    await expect(popup.getByText("Waiting for you", { exact: true })).toBeVisible();
    await expect(popup.getByText("This request also needs an approval outside Chrome. Answer it in Haarness.")).toBeVisible();
    await expect(popup.getByRole("button", { name: "Allow in this chat" })).toHaveCount(0);
    await expect(popup.getByRole("button", { name: "Deny" })).toHaveCount(0);
    await shoot(popup, `popup-answer-in-haarness-${theme}`);
  });

  test(`popup empty and stopped (${theme})`, async ({ bridge, worker, openExtensionPage }) => {
    await bridge.wait((message) => message.type === "hello");
    bridge.start();
    bridge.attach();
    const popup = await openExtensionPage("popup.html", { dark });
    await expect(popup.getByRole("heading", { name: "No chats are using Chrome yet" })).toBeVisible();
    await shoot(popup, `popup-empty-${theme}`);
    await worker.evaluate(() => (globalThis as unknown as { haarness: { events: { stopAll(): void } } }).haarness.events.stopAll());
    await expect(popup.getByRole("heading", { name: "You stopped Haarness" })).toBeVisible();
    await shoot(popup, `popup-stopped-${theme}`);
  });

  test(`welcome page (${theme})`, async ({ bridge, site, openExtensionPage }) => {
    await busy(bridge, site);
    const welcome = await openExtensionPage("welcome.html", { dark, width: 1200, height: 900 });
    await expect(welcome.getByRole("heading", { name: "Use Chrome with Haarness" })).toBeVisible();
    await expect(welcome.getByRole("heading", { name: "Haarness is installed" })).toContainText("Done");
    await expect(welcome.getByRole("heading", { name: "A project is open" })).toContainText("Done");
    await expect(welcome.getByLabel("Language")).toHaveValue("en");
    await shoot(welcome, `welcome-${theme}`);
  });
}

test.describe("before setup", () => {
  test.use({ setUp: false });
  for (const dark of [false, true])
    test(`popup and welcome before setup (${dark ? "dark" : "light"})`, async ({ openExtensionPage }) => {
      const popup = await openExtensionPage("popup.html", { dark });
      await expect(popup.getByRole("heading", { name: "Finish setup" })).toBeVisible();
      await shoot(popup, `popup-setup-${dark ? "dark" : "light"}`);
      const welcome = await openExtensionPage("welcome.html", { dark, width: 1200, height: 900 });
      await expect(welcome.getByText("Run this once in a terminal, then come back:")).toBeVisible();
      await shoot(welcome, `welcome-setup-${dark ? "dark" : "light"}`);
    });
});

test("the welcome page switches language, and Arabic runs right to left", async ({ bridge, site, openExtensionPage }) => {
  await busy(bridge, site);
  const welcome = await openExtensionPage("welcome.html", { width: 1200, height: 900 });
  await welcome.getByLabel("Language").selectOption("ar");
  await expect(welcome.locator("html")).toHaveAttribute("dir", "rtl");
  await expect(welcome.locator("html")).toHaveAttribute("lang", "ar");
  await shoot(welcome, "welcome-ar");
  const popup = await openExtensionPage("popup.html");
  await expect(popup.locator("html")).toHaveAttribute("dir", "rtl");
  await shoot(popup, "popup-ar");
  await welcome.locator("#language").selectOption("en");
  await expect(welcome.locator("html")).toHaveAttribute("dir", "ltr");
});

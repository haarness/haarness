import {
  test,
  expect,
  _electron as electron,
  chromium,
} from "@playwright/test";
import { execFileSync } from "node:child_process";
import { mkdtemp, mkdir, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

// Opt in with a local Cloud stack whose OIDC issuer is the checked-in test fixture.
const api = process.env.HAARNESS_ACCOUNT_E2E_URL;
test("Haarness account signs in through a browser, discovers Cloud models, and signs out", async () => {
  test.skip(
    !api,
    "Set HAARNESS_ACCOUNT_E2E_URL to the local fixture Cloud API",
  );
  test.setTimeout(180000);
  expect(new URL(api!).hostname).toBe("127.0.0.1");
  const root = await mkdtemp(join(tmpdir(), "haarness-account-desktop-"));
  const workspace = join(root, "project");
  await mkdir(workspace);
  const env: Record<string, string> = {
    ...Object.fromEntries(
      Object.entries(process.env).filter(
        (entry): entry is [string, string] => entry[1] !== undefined,
      ),
    ),
    HAARNESS_BIN: resolve(
      process.env.HAARNESS_BIN ?? "../../target/debug/haarness",
    ),
    HAARNESS_DATA_DIR: join(root, "data"),
    HAARNESS_CLOUD_CONFIG_DIR: join(root, "account"),
    HAARNESS_DESKTOP_DATA: join(root, "desktop"),
    HAARNESS_CLOUD_API_URL: api!,
    HAARNESS_DESKTOP_TEST: "1",
    HAARNESS_TEST_PROVIDER: "1",
  };
  delete env.ELECTRON_RUN_AS_NODE;
  const app = await electron.launch({ args: ["."], env });
  const browser = await chromium.launch();
  const page = await app.firstWindow();
  const browserPage = await browser.newPage();
  try {
    await page
      .getByRole("button", { name: "Sign in to Haarness", exact: true })
      .click();
    const dialog = page.getByRole("dialog", {
      name: "Haarness account",
      exact: true,
    });
    await expect(dialog).toBeVisible();
    await page.evaluate(() => {
      (window as any).accountSignInUrl = "";
      window.haarness.onEvent((event) => {
        if (
          event.kind === "menu" &&
          (event.data as any).action === "account-login"
        )
          (window as any).accountSignInUrl = (event.data as any).url;
      });
    });
    await dialog
      .getByRole("button", { name: "Sign in to Haarness", exact: true })
      .click();
    await expect
      .poll(() => page.evaluate(() => (window as any).accountSignInUrl))
      .not.toBe("");
    await browserPage.goto(
      await page.evaluate(() => (window as any).accountSignInUrl),
    );
    await browserPage.getByTestId("google-sign-in").click();
    await browserPage
      .getByRole("button", { name: "Continue with test account" })
      .click();
    await browserPage
      .getByRole("button", { name: "Approve", exact: true })
      .click();
    await expect(
      dialog.getByRole("button", { name: "Sign out", exact: true }),
    ).toBeVisible();
    await expect(
      dialog.getByText("Cloud workspaces", { exact: true }),
    ).toBeVisible();
    await expect(dialog.getByRole("alert")).toHaveCount(0);
    const connection = await page.evaluate(
      (directory) =>
        window.haarness.connect({ workspace: directory, trust: false }),
      workspace,
    );
    expect(connection).not.toBeNull();
    const providers = await page.evaluate(
      (id) => window.haarness.api<any[]>(id!, "refreshProviders"),
      connection!.id,
    );
    const cloud = providers.find((p) => p.id === "haarness");
    expect(cloud?.authenticated).toBe(true);
    // OIDC fixtures also support a live provider pricebook. Account discovery
    // must work with that catalog rather than depend on a test model being sold.
    expect(cloud?.models.length).toBeGreaterThan(0);
    await page.screenshot({
      path: test.info().outputPath("account-signed-in.png"),
    });
    const saved = JSON.parse(
      await readFile(join(root, "account/credentials.json"), "utf8"),
    );
    expect(saved.accessToken).toMatch(/^hca_/);
    expect(saved.refreshToken).toMatch(/^hcrt_/);
    const view = await page.evaluate(() =>
      window.haarness.account({ action: "status" }),
    );
    expect(view.loggedIn).toBe(true);
    const outside = async () => {
      const box = await dialog.boundingBox();
      if (!box || box.x < 8) throw Error("Expected an exposed account backdrop");
      await page.mouse.click(box.x / 2, box.y + box.height / 2);
      await expect(dialog).toHaveCount(0);
    };
    await outside();
    await page.getByRole("button", {
      name: view.user?.displayName || "Haarness account", exact: true,
    }).click();
    await expect(dialog.getByRole("button", { name: "Sign out", exact: true })).toBeVisible();
    const spaces = await page.evaluate(() =>
      window.haarness.account({ action: "workspaces" }),
    );
    const running = spaces.items.find(
      (w: any) => w.observedState === "running",
    );
    if (running) {
      const remote = await page.evaluate(
        (id) => window.haarness.connect({ cloudWorkspace: id }),
        running.id,
      );
      expect(remote?.remote).toBe(true);
      expect(remote?.cloudWorkspace).toBe(running.id);
      const info = await page.evaluate(
        (id) => window.haarness.api<any>(id!, "info"),
        remote!.id,
      );
      expect(info.name).toBe("haarness");
      if (process.env.HAARNESS_ACCOUNT_E2E_RENEW === "1") {
        // Run against the local stack configured with a 45-second attach ticket.
        await new Promise((resolve) => setTimeout(resolve, 50000));
        const renewed = await page.evaluate(
          (id) => window.haarness.api<any>(id!, "info"),
          remote!.id,
        );
        expect(renewed.name).toBe("haarness");
      }
    }
    expect(JSON.stringify(view)).not.toContain(saved.refreshToken);
    await dialog.getByRole("button", { name: "Sign out", exact: true }).click();
    await expect(
      dialog.getByRole("button", { name: "Sign in to Haarness", exact: true }),
    ).toBeVisible();
    expect(
      (await page.evaluate(() => window.haarness.account({ action: "status" })))
        .loggedIn,
    ).toBe(false);
    const response = await fetch(`${api}/api/v1/me`, {
      headers: { Authorization: `Bearer ${saved.accessToken}` },
    });
    expect(response.status).toBe(401);
    await dialog.getByRole("button", { name: "Close dialog" }).click();
    // Reopening and canceling a login must leave the account signed out.
    await page
      .getByRole("button", { name: "Sign in to Haarness", exact: true })
      .click();
    await dialog
      .getByRole("button", { name: "Sign in to Haarness", exact: true })
      .click();
    await expect(
      dialog.getByRole("button", { name: "Cancel", exact: true }),
    ).toBeVisible();
    await dialog.getByRole("button", { name: "Cancel", exact: true }).click();
    await expect(
      dialog.getByRole("button", { name: "Sign in to Haarness", exact: true }),
    ).toBeEnabled();
    // Outside dismissal uses the same cancellation path during browser login.
    await dialog.getByRole("button", { name: "Sign in to Haarness", exact: true }).click();
    await expect(dialog.getByRole("button", { name: "Cancel", exact: true })).toBeVisible();
    await outside();
    await page.getByRole("button", { name: "Sign in to Haarness", exact: true }).click();
    await expect(dialog.getByRole("button", { name: "Sign in to Haarness", exact: true })).toBeEnabled();
    expect((await page.evaluate(() => window.haarness.account({ action: "status" }))).loggedIn).toBe(false);
  } finally {
    await browser.close();
    await app.close();
    try {
      execFileSync(
        env.HAARNESS_BIN!,
        ["--workspace", workspace, "serve", "--stop"],
        { env, stdio: "ignore" },
      );
    } catch {}
    await rm(root, { recursive: true, force: true });
  }
});

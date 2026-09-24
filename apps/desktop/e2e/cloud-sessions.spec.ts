import { test, expect, _electron as electron } from "@playwright/test";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

// Opt-in live check; uses existing CLI sign-in without copying or revoking its credentials.
test("shared account lists templates and opens a live Cloud session", async () => {
  const session = process.env.HAARNESS_SESSION_E2E_ID;
  test.skip(!session, "Set HAARNESS_SESSION_E2E_ID to a running disposable Cloud session");
  const root = await mkdtemp(join(tmpdir(), "haarness-session-desktop-"));
  const env: Record<string,string> = { ...Object.fromEntries(Object.entries(process.env).filter((e):e is [string,string] => e[1] !== undefined)),
    HAARNESS_BIN: resolve(process.env.HAARNESS_BIN ?? "../../target/debug/haarness"), HAARNESS_DESKTOP_DATA: join(root,"desktop"), HAARNESS_DATA_DIR: join(root,"data"), HAARNESS_DESKTOP_TEST:"1" };
  delete env.ELECTRON_RUN_AS_NODE;
  const app = await electron.launch({args:["."],env});
  try {
    const page = await app.firstWindow();
    const account = await page.evaluate(() => window.haarness.account({action:"status"}));
    expect(account.loggedIn).toBe(true);
    await page.getByRole("button", {name:account.user.displayName,exact:true}).click();
    const dialog = page.getByRole("dialog",{name:"Haarness account",exact:true});
    await expect(dialog.getByRole("heading",{name:"Cloud sessions",exact:true})).toBeVisible();
    await dialog.getByRole("button",{name:"New session",exact:true}).click();
    await expect(dialog.getByRole("combobox",{name:"Cloud agent",exact:true})).toBeVisible();
    await expect(dialog.getByRole("button",{name:"Manage agent templates",exact:true})).toBeVisible();
    const sessions = await page.evaluate(() => window.haarness.account({action:"sessions"}));
    const selected = sessions.items.find((s:any) => s.id===session);
    expect(selected.workspace.observedState).toBe("running");
    const row = dialog.locator(".account-workspace").filter({hasText:selected.title});
    await row.getByRole("button",{name:"Open",exact:true}).click();
    await expect(dialog).not.toBeVisible();
    const recents = await page.evaluate(() => window.haarness.boot());
    const saved = recents.recent.find((r:any) => r.cloudSession===session);
    expect(saved?.cloudWorkspace).toBe(selected.workspace.id);
    expect(saved?.label).toBe(selected.title);
    const reopened = await page.evaluate((id) => window.haarness.connect({recent:id}), saved!.id);
    expect(reopened?.cloudSession).toBe(session);
    expect(reopened?.remote).toBe(true);
    expect(reopened?.info.defaultAgent).toBe("cloud-agent");
    const info = await page.evaluate((id) => window.haarness.api<any>(id!,"info"),reopened!.id);
    expect(info.name).toBe("haarness");
    await page.screenshot({path:test.info().outputPath("cloud-session-connected.png")});
  } finally { await app.close(); await rm(root,{recursive:true,force:true}); }
});

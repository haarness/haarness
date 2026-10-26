import { test, expect, _electron as electron } from "@playwright/test";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

// Opt-in live check; uses existing CLI sign-in without copying or revoking its credentials.
test("shared account lists templates and opens a live Cloud workspace", async () => {
  const workspace = process.env.HAARNESS_WORKSPACE_E2E_ID;
  test.skip(!workspace, "Set HAARNESS_WORKSPACE_E2E_ID to a running disposable Cloud workspace");
  const root = await mkdtemp(join(tmpdir(), "haarness-workspace-desktop-"));
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
    await expect(dialog.getByRole("heading",{name:"Cloud workspaces",exact:true})).toBeVisible();
    await dialog.getByRole("button",{name:"New workspace",exact:true}).click();
    await expect(dialog.getByRole("combobox",{name:"Use Cloud agent (optional)",exact:true})).toBeVisible();
    await expect(dialog.getByRole("combobox",{name:"Use Cloud agent (optional)",exact:true})).toHaveAttribute("data-value","");
    await expect(dialog.getByRole("textbox",{name:"Workspace instructions",exact:true})).toBeVisible();
    await dialog.getByText("Advanced workspace configuration", {exact:true}).click();
    await expect(dialog.getByRole("textbox",{name:"Workspace configuration JSON",exact:true})).toBeVisible();
    await expect(dialog.getByRole("button",{name:"Manage agent templates",exact:true})).toBeVisible();
    const workspaces = await page.evaluate(() => window.haarness.account({action:"workspaces"}));
    const selected = workspaces.items.find((w:any) => w.id===workspace);
    expect(selected.observedState).toBe("running");
    const row = dialog.locator(".account-workspace").filter({hasText:selected.title});
    await row.getByRole("button",{name:"Open",exact:true}).click();
    await expect(dialog).not.toBeVisible();
    const recents = await page.evaluate(() => window.haarness.boot());
    const saved = recents.recent.find((r:any) => r.cloudWorkspace===workspace);
    expect(saved).toBeTruthy();
    expect(saved?.label).toBe(selected.title);
    const reopened = await page.evaluate((id) => window.haarness.connect({recent:id}), saved!.id);
    expect(reopened?.cloudWorkspace).toBe(workspace);
    expect(reopened?.remote).toBe(true);
    expect(reopened?.info.defaultAgent).toBe("cloud-agent");
    const info = await page.evaluate((id) => window.haarness.api<any>(id!,"info"),reopened!.id);
    expect(info.name).toBe("haarness");
    await page.screenshot({path:test.info().outputPath("cloud-workspace-connected.png")});
  } finally { await app.close(); await rm(root,{recursive:true,force:true}); }
});

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
import { reasoningLabel } from "../src/renderer/reasoning";

// Opt in with a local Cloud stack whose OIDC issuer is the checked-in test fixture.
const api = process.env.HAARNESS_ACCOUNT_E2E_URL;
test("Haarness account signs in through a browser, discovers Cloud models, and signs out", async () => {
  test.skip(
    !api,
    "Set HAARNESS_ACCOUNT_E2E_URL to the local fixture Cloud API",
  );
  test.setTimeout(process.env.HAARNESS_ACCOUNT_E2E_REASONING === "1" ? 600000 : 180000);
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
      dialog.getByText("Cloud sessions", { exact: true }),
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
    if (process.env.HAARNESS_ACCOUNT_E2E_BILLING === "1") {
      const walletBefore = await page.evaluate(() => window.haarness.account({ action: "billing" }));
      await app.evaluate(({ dialog }, directory) => {
        const original = dialog.showOpenDialog;
        dialog.showOpenDialog = async () => {
          dialog.showOpenDialog = original;
          return { canceled: false, filePaths: [directory] };
        };
      }, workspace);
      await page.getByRole("region", { name: "Project home" }).getByRole("button", { name: "Open another project", exact: true }).click();
      await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true");
      const model = page.getByRole("combobox", { name: "Model", exact: true });
      await model.click();
      const list = page.locator(`[id=${JSON.stringify(await model.getAttribute("aria-controls"))}]`);
      await list.locator('[role="option"][data-value="haarness/openai/gpt-4.1-nano"]').click();
      await page.locator(".select-footer").getByRole("button", { name: "Done", exact: true }).click();
      await page.getByRole("textbox", { name: "Message", exact: true }).fill("Without tools, reply with exactly DESKTOP_BILLING_OK");
      await page.getByRole("button", { name: "Send message", exact: true }).click();
      await expect(page.locator(".message.assistant").filter({ hasText: "DESKTOP_BILLING_OK" })).toBeVisible({ timeout: 120000 });
      await expect(page.getByRole("button", { name: "Stop", exact: true })).toHaveCount(0, { timeout: 120000 });
      await page.getByRole("button", { name: view.user?.displayName || "Haarness account", exact: true }).click();
      await expect(dialog.getByRole("heading", {name:"Plan and credits"})).toBeVisible();
      await expect(dialog.getByRole("button", { name:"Add credits", exact:true })).toBeVisible();
      await expect.poll(async () => {
        const wallet = await page.evaluate(() => window.haarness.account({ action: "billing" }));
        return BigInt(wallet.includedAi.nanos) < BigInt(walletBefore.includedAi.nanos) && wallet.heldReserve.nanos === "0" && wallet.purchased.nanos === walletBefore.purchased.nanos;
      }).toBe(true);
      const wallet = await page.evaluate(() => window.haarness.account({ action: "billing" }));
      const { credits } = await import("../src/billing");
      await expect(dialog.getByTestId("included-credits")).toHaveText(credits(wallet.includedAi.credits));
      await expect(dialog.getByTestId("purchased-credits")).toHaveText(credits(wallet.purchased.credits));
      await page.screenshot({ path: test.info().outputPath("billing-live.png") });
      await outside();
    }
    if (process.env.HAARNESS_ACCOUNT_E2E_REASONING === "1") {
      // Real Cloud catalog -> IPC -> model picker -> reasoning selection ->
      // streamed native turn, using only this test's account and workspace.
      await app.evaluate(({ dialog }, directory) => {
        const original = dialog.showOpenDialog;
        dialog.showOpenDialog = async () => {
          dialog.showOpenDialog = original;
          return { canceled: false, filePaths: [directory] };
        };
      }, workspace);
      await page.getByRole("region", { name: "Project home" })
        .getByRole("button", { name: "Open another project", exact: true }).click();
      await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true");
      const cases = [
        ["openai/gpt-6.1-sol", ["low", "medium", "high", "xhigh", "max"]],
        ["anthropic/claude-sonnet-5.5", ["low", "medium", "high", "xhigh", "max"]],
        ["anthropic/claude-haiku-5.5", ["low", "medium", "high", "xhigh", "max"]],
        ["openai/gpt-6-astra", ["low", "medium", "high", "xhigh", "max"]],
        ["anthropic/claude-sonnet-4.6", ["low", "medium", "high", "max"]],
        ["anthropic/claude-haiku-4.5", ["low", "medium", "high"]],
        ["google/gemini-4-argon", ["low", "medium", "high"]],
        ["google/gemini-3.8-flash", ["low", "medium", "high"]],
        ["google/gemini-3.5-flash-lite", ["minimal", "low", "medium", "high"]],
      ] as const;
      const model = page.getByRole("combobox", { name: "Model", exact: true });
      for (const [id, efforts] of cases) {
        const info = cloud.models.find((m: any) => m.id === id);
        expect(info, `Live Cloud model ${id}`).toBeTruthy();
        expect(info.reasoningEfforts).toEqual([...efforts]);
        await model.click();
        const list = page.locator(`[id=${JSON.stringify(await model.getAttribute("aria-controls"))}]`);
        await list.locator(`[role="option"][data-value=${JSON.stringify(`haarness/${id}`)}]`).click();
        const slider = page.getByRole("slider", { name: "Reasoning effort", exact: true });
        await expect(slider).toHaveAttribute("max", String(efforts.length));
        // Exercise every advertised UI level and choose low for a bounded call.
        await slider.focus();
        await slider.press("Home");
        for (const effort of efforts) {
          await slider.press("ArrowRight");
          await expect(slider).toHaveAttribute("aria-valuetext", reasoningLabel(effort));
        }
        await expect(slider).toHaveValue(String(efforts.length));
        await page.screenshot({ animations: "disabled", path: test.info().outputPath(`reasoning-${id.replaceAll("/", "-")}.png`) });
        await slider.press("Home");
        for (let n = 0; n <= [...efforts as readonly string[]].indexOf("low"); n++) await slider.press("ArrowRight");
        await page.locator(".select-footer").getByRole("button", { name: "Done", exact: true }).click();
        // Catalog/selection coverage also includes retired or overloaded models.
        // Live calls use the explicitly selected available test models.
        const liveModels = (process.env.HAARNESS_ACCOUNT_E2E_LIVE_MODELS ?? "openai/gpt-6.1-sol,anthropic/claude-haiku-5.5,anthropic/claude-haiku-4.5,google/gemini-3.5-flash-lite").split(",");
        if (!liveModels.includes(id)) continue;
        const marker = `CLOUD_REASONING_OK_${id.replaceAll(/[^a-z0-9]/g, "_")}`;
        await page.getByRole("textbox", { name: "Message", exact: true }).fill(`Without tools, reply with exactly ${marker}`);
        await page.getByRole("button", { name: "Send message", exact: true }).click();
        await expect(page.locator(".message.assistant").filter({ hasText: marker })).toBeVisible({ timeout: 120000 });
        await expect(page.getByRole("button", { name: "Stop", exact: true })).toHaveCount(0, { timeout: 120000 });
        await page.getByRole("button", { name: "New chat", exact: true }).click();
      }
      expect(cloud.models.find((m: any) => m.id === "openai/gpt-4.1").reasoningEfforts).toEqual([]);
      // Image-only models (Nano Banana) are not chat models.
      expect(cloud.models.filter((m: any) => /nano-banana|-image/.test(m.id))).toEqual([]);
    }
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

test("unified API key connects Desktop with its actual capabilities", async () => {
  test.skip(!api || !process.env.HAARNESS_API_KEY, "Provide a disposable local Cloud API key");
  test.setTimeout(180000);
  expect(new URL(api!).hostname).toBe("127.0.0.1");
  const root=await mkdtemp(join(tmpdir(),"haarness-key-desktop-"));
  const workspace=join(root,"project");await mkdir(workspace);
  execFileSync("git",["init","-q",workspace]);
  const env: Record<string,string>={...Object.fromEntries(Object.entries(process.env).filter((entry):entry is [string,string]=>entry[1]!==undefined)),HAARNESS_BIN:resolve(process.env.HAARNESS_BIN??"../../target/debug/haarness"),HAARNESS_DATA_DIR:join(root,"data"),HAARNESS_CLOUD_CONFIG_DIR:join(root,"account"),HAARNESS_DESKTOP_DATA:join(root,"desktop"),HAARNESS_CLOUD_API_URL:api!,HAARNESS_DESKTOP_TEST:"1"};
  delete env.ELECTRON_RUN_AS_NODE;
  const app=await electron.launch({args:["."],env});
  try {
    const page=await app.firstWindow();
    const status=await page.evaluate(()=>window.haarness.account({action:"status"}));
    expect(status.authKind).toBe("api_key");
    await page.getByRole("button",{name:status.user.displayName,exact:true}).click();
    const dialog=page.getByRole("dialog",{name:"Haarness account",exact:true});
    await expect(dialog.getByText(/Connected with HAARNESS_API_KEY/)).toBeVisible();
    await expect(dialog.getByRole("button",{name:"Sign out",exact:true})).toHaveCount(0);
    const canRead=status.scopes.includes("workspaces:read"),canBill=status.scopes.includes("billing:read");
    await expect(dialog.getByRole("heading",{name:"Cloud sessions",exact:true})).toHaveCount(canRead?1:0);
    if(canBill)await expect(dialog.getByTestId("included-credits")).toBeVisible();
    else await expect(dialog.getByText("This API key does not include billing access.",{exact:true})).toBeVisible();
    await expect(dialog.getByRole("alert")).toHaveCount(0);
    await page.screenshot({path:test.info().outputPath(canRead?"api-key-connected.png":"api-key-models-only.png")});
    await dialog.getByRole("button",{name:"Done",exact:true}).click();
    const connection=await page.evaluate(directory=>window.haarness.connect({workspace:directory,trust:false}),workspace);
    const providers=await page.evaluate(id=>window.haarness.api<any[]>(id!,"refreshProviders"),connection!.id);
    const provider=providers.find(p=>p.id==="haarness");expect(provider?.authenticated).toBe(true);expect(provider?.models.length).toBeGreaterThan(0);
    if(status.scopes.includes("models:invoke")) {
      const reply=await page.evaluate(async id=>{
        const thread=await window.haarness.api<any>(id!,"createThread",[{title:"Desktop unified key",provider:"haarness",model:"openai/gpt-4.1-nano"}]);
        const turn=await window.haarness.api<any>(id!,"startTurn",[thread.id,{requestId:crypto.randomUUID(),text:"Reply DESKTOP_UNIFIED_KEY_OK, without tools."}]);
        return {thread:thread.id,turn:turn.id};
      },connection!.id);
      await expect.poll(async ()=>await page.evaluate(async ({id,thread})=>{
        const items=await window.haarness.api<any[]>(id!,"listItems",[thread]);return items.some(i=>i.data?.type==="assistant_message"&&i.data.text.includes("DESKTOP_UNIFIED_KEY_OK"));
      },{id:connection!.id,thread:reply.thread}),{timeout:120000}).toBe(true);
    }
    await expect(readFile(join(root,"account/credentials.json"))).rejects.toThrow();
  } finally {
    await app.close();
    execFileSync(env.HAARNESS_BIN,["--workspace",workspace,"serve","--stop"],{env,stdio:"ignore"});
    await rm(root,{recursive:true,force:true});
  }
});

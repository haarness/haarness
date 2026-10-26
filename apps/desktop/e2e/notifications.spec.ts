import {
  test,
  expect,
  _electron as electron,
  type ElectronApplication,
  type Page,
} from "@playwright/test";
import { mkdtemp, mkdir, writeFile, rm, realpath } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { execFileSync } from "node:child_process";
import { Haarness, type Thread } from "@haarness/sdk";
import { stopChatHome } from "./fixtures/chat-home";
const binary = resolve(
  process.env.HAARNESS_BIN ?? "../../target/debug/haarness",
);
let app: ElectronApplication, page: Page, temporary: string;
let env: Record<string, string>;
const directories: string[] = [],
  clients: Haarness[] = [],
  errors: string[] = [];
let first: Thread, second: Thread, remote: Thread, remoteId: string;
test.describe.configure({ mode: "serial" });
const composer = () =>
  page.getByRole("textbox", { name: "Message", exact: true });
const project = (name: string) =>
  page.getByRole("region", { name: `Project ${name}`, exact: true });
async function background() {
  const win = await app.browserWindow(page);
  await win.evaluate((win) => win.hide());
  await expect.poll(() => win.evaluate((win) => win.isFocused())).toBe(false);
}
async function records() {
  return app.evaluate(
    () =>
      (globalThis as any).notificationRecords.map((n: any) => ({
        title: n.title,
        body: n.body,
      })) as { title: string; body: string }[],
  );
}
async function clearRecords() {
  await app.evaluate(() => {
    (globalThis as any).notificationRecords = [];
  });
}
async function notification(title: string, kind = "Task finished") {
  await expect
    .poll(
      async () =>
        (await records()).filter(
          (n) => n.body.startsWith(title + " · ") && n.title.includes(kind),
        ).length,
    )
    .toBe(1);
  return (await records()).findIndex(
    (n) => n.body.startsWith(title + " · ") && n.title.includes(kind),
  );
}
async function clickNotification(index: number) {
  await app.evaluate(
    (_, index) => (globalThis as any).notificationRecords[index].emit("click"),
    index,
  );
}
async function turn(client: Haarness, thread: Thread, text: string) {
  const value = await client.startTurn(thread.id, {
    text,
    requestId: crypto.randomUUID(),
  });
  return client.waitTurn(value.id);
}
async function select(name: string, title: string) {
  const expand = project(name).getByRole("button", {
    name: `Expand project ${name}`,
    exact: true,
  });
  if (await expand.count()) await expand.click();
  await project(name).getByRole("button", { name: title, exact: true }).click();
  await expect(project(name)).toHaveClass(/current/);
}
async function selected(connection: string, thread: Thread, marker: string) {
  await expect
    .poll(() =>
      page
        .evaluate(() => window.haarness.storage("active-project"))
        .catch(() => null),
    )
    .toBe(connection);
  await expect
    .poll(() =>
      page
        .evaluate((id) => window.haarness.storage("session:" + id), connection)
        .catch(() => null),
    )
    .toBe(thread.id);
  await expect(page.locator(".transcript")).toContainText(marker);
  await expect(page.locator(".application")).toHaveAttribute(
    "data-project-ready",
    "true",
  );
}
test.beforeAll(async () => {
  test.setTimeout(240000);
  temporary = await realpath(
    await mkdtemp(join(tmpdir(), "haarness-notifications-")),
  );
  env = Object.fromEntries(
    Object.entries(process.env).filter(
      (entry): entry is [string, string] => entry[1] !== undefined,
    ),
  );
  Object.assign(env, {
    HAARNESS_BIN: binary,
    HAARNESS_HOME: join(temporary, "home"),
    HAARNESS_DATA_DIR: join(temporary, "state"),
    HAARNESS_DESKTOP_DATA: join(temporary, "desktop"),
    HAARNESS_TEST_PROVIDER: "1",
    HAARNESS_DESKTOP_TEST: "1",
  });
  delete env.ELECTRON_RUN_AS_NODE;
  for (const name of ["alpha", "beta", "remote 日本"]) {
    const directory = join(temporary, name);
    directories.push(directory);
    await mkdir(join(directory, ".haarness"), { recursive: true });
    // Review, so an approval waits for a person rather than the Auto reviewer.
    await writeFile(
      join(directory, ".haarness/config.toml"),
      '[defaults]\nprovider="echo"\nmodel="echo-1"\napproval_mode="review"\n[permissions]\nbash="ask"\ntask="allow"\n[providers.notification-failure]\nprotocol="anthropic-messages"\nbase_url="http://127.0.0.1:1/v1"\nmodels=["fixture"]\napi_key_env="HAARNESS_NOTIFICATION_MISSING_KEY"\n',
    );
    await writeFile(join(directory, "fixture.txt"), name + "\n");
    for (const args of [
      ["init", "-b", "main"],
      ["add", "."],
      [
        "-c",
        "user.name=Notification Fixture",
        "-c",
        "user.email=fixture@example.test",
        "commit",
        "-m",
        "Fixture",
      ],
    ])
      execFileSync("git", args, { cwd: directory, stdio: "ignore" });
  }
  app = await electron.launch({ args: ["."], env, timeout: 120000 });
  page = await app.firstWindow();
  page.on("pageerror", (error) => errors.push(error.message));
  await page.setViewportSize({ width: 1420, height: 940 });
  // Only OS delivery is replaced. Real Electron Notification instances retain
  // production click listeners, IPC, project routing and server subscriptions.
  await app.evaluate(({ Notification }) => {
    (globalThis as any).notificationRecords = [];
    Notification.isSupported = () => true;
    Notification.prototype.show = function () {
      (globalThis as any).notificationRecords.push(this);
    };
    Notification.prototype.close = function () {
      this.emit("close");
    };
  });
  for (const [index, directory] of directories.slice(0, 2).entries()) {
    await app.evaluate(({ dialog }, directory) => {
      dialog.showOpenDialog = async () => ({
        canceled: false,
        filePaths: [directory],
      });
    }, directory);
    await page
      .getByRole("button", { name: "Open another project", exact: true })
      .click();
    await expect(project(index ? "beta" : "alpha")).toHaveClass(/current/);
    await expect(page.locator(".application")).toHaveAttribute(
      "data-project-ready",
      "true",
    );
    const endpoint = JSON.parse(
      execFileSync(
        binary,
        ["--workspace", directory, "--trust-project", "desktop"],
        {
          env,
          input: '{"action":"connect"}',
          encoding: "utf8",
          timeout: 30000,
        },
      ),
    );
    clients.push(
      new Haarness({ baseUrl: endpoint.url, token: endpoint.token }),
    );
  }
  await page.evaluate(() =>
    window.haarness.preferences({ notifications: false }),
  );
  first = await clients[0].createThread({
    title: "Alpha conversation",
    provider: "echo",
    model: "echo-1",
  });
  second = await clients[1].createThread({
    title: "Beta conversation",
    provider: "echo",
    model: "echo-1",
  });
  await turn(clients[0], first, "alpha initial transcript");
  await turn(clients[1], second, "beta initial transcript");
  await select("beta", second.title);
  const endpoint = JSON.parse(
    execFileSync(
      binary,
      ["--workspace", directories[0], "--trust-project", "desktop"],
      { env, input: '{"action":"connect"}', encoding: "utf8", timeout: 30000 },
    ),
  );
  const view = await page.evaluate((input) => window.haarness.connect(input), {
    url: endpoint.url,
    token: endpoint.token,
    workspace: directories[2],
    label: "Fixture server",
  });
  remoteId = view!.id;
  clients.push(
    new Haarness({
      baseUrl: endpoint.url + "/v1/host/workspaces/" + view!.remoteWorkspace,
      token: endpoint.token,
    }),
  );
  remote = await clients[2].createThread({
    title: "Remote conversation 日本",
    provider: "echo",
    model: "echo-1",
  });
  await turn(clients[2], remote, "remote isolated transcript");
  await page.reload();
  await selected("local:" + directories[1], second, "beta initial transcript");
  await page.evaluate(() =>
    window.haarness.preferences({ notifications: true }),
  );
  await clearRecords();
});
test.afterAll(async () => {
  for (const client of clients) client.close();
  await app?.close().catch(() => {});
  for (const directory of directories)
    try {
      execFileSync(binary, ["--workspace", directory, "serve", "--stop"], {
        env,
        stdio: "ignore",
        timeout: 30000,
      });
    } catch {}
  stopChatHome(binary, env);
  if (temporary) await rm(temporary, { recursive: true, force: true });
});
test("background completion opens a collapsed project and preserves both conversation drafts", async () => {
  await select("alpha", first.title);
  await composer().fill("unsent alpha draft 日本");
  await select("beta", second.title);
  await composer().fill("unsent beta draft");
  await project("alpha")
    .getByRole("button", { name: "Collapse project alpha", exact: true })
    .click();
  await expect(project("alpha").locator(".sessions")).toHaveCount(0);
  await background();
  expect(
    (await turn(clients[0], first, "alpha background completed")).status,
  ).toBe("completed");
  await clickNotification(await notification(first.title));
  await selected(
    "local:" + directories[0],
    first,
    "Echo: alpha background completed",
  );
  await expect(composer()).toHaveValue("unsent alpha draft 日本");
  const win = await app.browserWindow(page);
  expect(
    await win.evaluate((win) => win.isVisible() && !win.isMinimized()),
  ).toBe(true);
  await expect.poll(() => win.evaluate((win) => win.isFocused())).toBe(true);
  await select("beta", second.title);
  await expect(composer()).toHaveValue("unsent beta draft");
  expect(
    (await records()).filter((n) => n.body.startsWith(first.title + " · ")),
  ).toHaveLength(1);
  await page.screenshot({
    path: test.info().outputPath("draft-preservation.png"),
  });
});
test("remote notification switches to the exact server project and returns to the local draft", async () => {
  await background();
  await turn(clients[2], remote, "remote notification result 日本");
  await clickNotification(await notification(remote.title));
  await selected(remoteId, remote, "Echo: remote notification result 日本");
  await expect(
    page.getByRole("button", { name: "Switch server", exact: true }),
  ).toContainText("Fixture server");
  expect(
    (await clients[0].listThreads()).some((thread) => thread.id === remote.id),
  ).toBe(false);
  await composer().fill("remote draft kept separately");
  await clearRecords();
  await background();
  await turn(clients[1], second, "beta return from remote");
  await clickNotification(await notification(second.title));
  await selected(
    "local:" + directories[1],
    second,
    "Echo: beta return from remote",
  );
  await expect(composer()).toHaveValue("unsent beta draft");
  await page.screenshot({ path: test.info().outputPath("remote-return.png") });
});
test("notifications respect focus and preference, and interrupted turns do not report success", async () => {
  await clearRecords();
  const win = await app.browserWindow(page);
  await win.evaluate((win) => {
    win.show();
    win.focus();
  });
  await expect.poll(() => win.evaluate((win) => win.isFocused())).toBe(true);
  await turn(clients[0], first, "foreground stays quiet");
  await page.evaluate(() =>
    window.haarness.preferences({ notifications: false }),
  );
  await background();
  await turn(clients[0], first, "muted stays quiet");
  await page.waitForTimeout(300);
  expect(await records()).toHaveLength(0);
  await page.evaluate(() =>
    window.haarness.preferences({ notifications: true }),
  );
  const running = await clients[0].startTurn(first.id, {
    text: "::mdslow",
    requestId: crypto.randomUUID(),
  });
  await clients[0].interruptTurn(running.id);
  expect((await clients[0].waitTurn(running.id)).status).toBe("interrupted");
  await page.waitForTimeout(300);
  expect(await records()).toHaveLength(0);
});
test("approval and question notifications open actionable conversations", async () => {
  await clearRecords();
  await background();
  const approval = await clients[0].startTurn(first.id, {
    text: '!tool bash {"command":"printf notification-approval-proof"}',
    requestId: crypto.randomUUID(),
  });
  await clickNotification(await notification(first.title, "Approval needed"));
  await selected(
    "local:" + directories[0],
    first,
    "notification-approval-proof",
  );
  await page.getByRole("button", { name: "Allow once", exact: true }).click();
  expect((await clients[0].waitTurn(approval.id)).status).toBe("completed");
  await expect(page.locator(".transcript")).toContainText("Tool bash =>");
  await clearRecords();
  await background();
  const question = await clients[2].startTurn(remote.id, {
    text: '!tool question {"prompt":"Notification question 日本","choices":["Continue"]}',
    requestId: crypto.randomUUID(),
  });
  await clickNotification(await notification(remote.title, "Answer needed"));
  await expect(page.locator(".question")).toContainText(
    "Notification question 日本",
  );
  await page
    .locator(".question")
    .getByRole("button", { name: "Continue", exact: true })
    .click();
  await page
    .locator(".question")
    .getByRole("button", { name: "Reply", exact: true })
    .click();
  expect((await clients[2].waitTurn(question.id)).status).toBe("completed");
  await expect(composer()).toHaveValue("remote draft kept separately");
});
test("a delegated question opens its parent conversation and child completion stays quiet", async () => {
  await clearRecords();
  await background();
  const parent = await clients[0].startTurn(first.id, {
    requestId: crypto.randomUUID(),
    text:
      "!tool task " +
      JSON.stringify({
        description: "Notification child",
        agent: "general",
        prompt:
          '!tool question {"prompt":"Delegated notification question","choices":["Continue"]}',
      }),
  });
  await expect
    .poll(async () => (await clients[0].listQuestions()).length)
    .toBe(1);
  const question = (await clients[0].listQuestions())[0];
  expect(question.threadId).not.toBe(first.id);
  expect(question.rootThreadId).toBe(first.id);
  await clickNotification(await notification(first.title, "Answer needed"));
  await selected("local:" + directories[0], first, "Notification child");
  await expect(page.locator(".question")).toContainText(
    "Delegated notification question",
  );
  await background();
  await clients[0].replyQuestion(question.questionId, { answer: "Continue" });
  expect((await clients[0].waitTurn(parent.id)).status).toBe("completed");
  await notification(first.title);
  expect(
    (await records()).filter((n) => n.title.includes("Task finished")),
  ).toHaveLength(1);
});
test("a notification survives reload and restores a minimized window without replay duplicates", async () => {
  await clearRecords();
  await background();
  await turn(clients[0], first, "reload destination marker");
  const index = await notification(first.title);
  const win = await app.browserWindow(page);
  await win.evaluate(async (win, index) => {
    const loading = new Promise<void>((resolve) =>
      win.webContents.once("did-start-navigation", () => resolve()),
    );
    win.reload();
    await loading;
    (globalThis as any).notificationRecords[index].emit("click");
  }, index);
  await selected(
    "local:" + directories[0],
    first,
    "Echo: reload destination marker",
  );
  await expect(composer()).toHaveValue("unsent alpha draft 日本");
  await win.evaluate((win) => win.minimize());
  await expect.poll(() => win.evaluate((win) => win.isMinimized())).toBe(true);
  await turn(clients[1], second, "minimized destination marker");
  await clickNotification(await notification(second.title));
  await selected(
    "local:" + directories[1],
    second,
    "Echo: minimized destination marker",
  );
  expect(await win.evaluate((win) => win.isMinimized())).toBe(false);
  expect(
    (await records()).filter((n) => n.body.startsWith(first.title + " · ")),
  ).toHaveLength(1);
});
test("failed and archived conversations open without changing their history", async () => {
  await clearRecords();
  await background();
  const failed = await clients[0].createThread({
    title: "Failed conversation",
    provider: "notification-failure",
    model: "fixture",
  });
  await clients[0]
    .startTurn(failed.id, {
      text: "Failure notification proof",
      requestId: crypto.randomUUID(),
    })
    .catch(() => undefined);
  const index = await notification(failed.title, "Task failed");
  await clients[0].updateThread(failed.id, { archived: true });
  await clickNotification(index);
  await selected(
    "local:" + directories[0],
    failed,
    "Failure notification proof",
  );
  expect(
    (await clients[0].listItems(failed.id)).some(
      (item) => item.data.type === "error",
    ),
  ).toBe(true);
  expect((await clients[0].getThread(failed.id)).archived).toBe(true);
  expect(
    (await records()).some(
      (n) =>
        n.body.startsWith(failed.title + " · ") &&
        n.title.includes("Task finished"),
    ),
  ).toBe(false);
});
test("a queue of prompts sends one finished banner, after its last turn, and none while it runs", async () => {
  await clearRecords();
  await background();
  const finished = async () =>
    (await records()).filter(
      (n) => n.body.startsWith(first.title + " · ") && n.title.includes("Task finished"),
    ).length;
  // Each queued prompt starts as its own root turn moments after the last
  // one finishes; none of those intermediate ends is "finished".
  const running = await clients[0].startTurn(first.id, {
    text: "::mdslow queue head",
    requestId: crypto.randomUUID(),
  });
  for (const text of ["queued follow-up one", "queued follow-up two"])
    await clients[0].queueInput(first.id, {
      text,
      requestId: crypto.randomUUID(),
      delivery: "queue",
    });
  await clients[0].waitTurn(running.id);
  await expect
    .poll(async () =>
      (await clients[0].listItems(first.id)).some(
        (item) =>
          item.data.type === "assistant_message" &&
          item.data.text.includes("queued follow-up two"),
      ),
    )
    .toBe(true);
  await expect.poll(finished).toBe(1);
  // Long past the grace: still exactly one banner for the whole queue.
  await page.waitForTimeout(1500);
  expect(await finished()).toBe(1);
});
test("rapid notification clicks select the last destination and replay subscriptions do not duplicate alerts", async () => {
  await clearRecords();
  await background();
  await turn(clients[0], first, "rapid alpha target");
  const a = await notification(first.title);
  await turn(clients[2], remote, "rapid remote target");
  const b = await notification(remote.title);
  await app.evaluate(
    (_, indexes) => {
      for (const index of indexes)
        (globalThis as any).notificationRecords[index].emit("click");
    },
    [a, b],
  );
  await selected(remoteId, remote, "Echo: rapid remote target");
  await expect(composer()).toHaveValue("remote draft kept separately");
  const replay = await page.evaluate(
    (id) => window.haarness.subscribe(id, 0),
    remoteId,
  );
  await page.waitForTimeout(300);
  await page.evaluate((id) => window.haarness.unsubscribe(id), replay);
  expect(await records()).toHaveLength(2);
});
test("clicking an existing notification recreates a closed macOS window and restores the correct chat", async () => {
  test.skip(
    process.platform !== "darwin",
    "macOS keeps the app alive after its last window closes",
  );
  await clearRecords();
  await background();
  await turn(clients[2], remote, "closed window destination");
  const index = await notification(remote.title);
  const win = await app.browserWindow(page);
  await win.evaluate((win) => win.close());
  await expect.poll(() => app.windows().length).toBe(0);
  const opened = app.waitForEvent("window");
  await clickNotification(index);
  page = await opened;
  page.on("pageerror", (error) => errors.push(error.message));
  await selected(remoteId, remote, "Echo: closed window destination");
  await expect(composer()).toHaveValue("remote draft kept separately");
  await page.screenshot({
    path: test.info().outputPath("reopened-conversation.png"),
  });
  expect(errors).toEqual([]);
});

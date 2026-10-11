import {
  test,
  expect,
  _electron as electron,
  type ElectronApplication,
  type Page,
} from "@playwright/test";
import {
  mkdtemp,
  mkdir,
  writeFile,
  readFile,
  realpath,
  copyFile,
} from "node:fs/promises";
import { tmpdir, homedir } from "node:os";
import { join, resolve, relative } from "node:path";
import { execFileSync } from "node:child_process";
import { createServer, type Server } from "node:http";
import { Haarness, type Thread, type Item } from "@haarness/sdk";
import { browserLab } from "./fixtures/browser-lab";

test.skip(
  process.env.HAARNESS_BROWSER_PROMPTS !== "1",
  "Opt-in natural-language browser evaluation",
);
let app: ElectronApplication,
  page: Page,
  site: Server,
  root: string,
  workspace: string,
  siteURL: string;
let client: Haarness,
  endpoint: { url: string; token: string },
  env: Record<string, string>,
  connection: string;
const model = process.env.HAARNESS_BROWSER_MODEL ?? "gpt-5.6-luna";
const sourceBinary = resolve(
  process.env.HAARNESS_BIN ?? "../../target/debug/haarness",
);
let binary = sourceBinary;
const evidence = resolve(
  process.env.HAARNESS_BROWSER_EVIDENCE ??
    "test-results/browser-prompts-evidence",
);
type Proof = {
  items: Item[];
  final: string;
  calls: string[];
  results: any[];
  thread: Thread;
  folder: string;
};
type Case = {
  id: string;
  prompts: (base: string) => string[];
  tui?: boolean;
  check: (proof: Proof) => Promise<void>;
};
/** Tabs once Desktop's browser is attached, else -1: servers answer a tab
 * list without a browser too, naming which browsers the chat can use. */
function attachedTabs(state: any): number {
  const haarness = state.browsers?.find(
    (entry: any) => entry.browser === "haarness",
  );
  return haarness?.available === false ? -1 : state.tabs.length;
}
async function tool(thread: Thread, name: string, args: object = {}) {
  const result = await client.callBrowserTool(
    thread.id,
    { name: "browser_" + name, arguments: args as any },
    { timeoutMs: 90000 },
  );
  if (result.isError) throw new Error(result.output);
  return JSON.parse(result.output);
}
function recordedResults(items: Item[]): any[] {
  return items
    .filter((item) => item.data.type === "tool_result")
    .flatMap((item) => {
      const result = item.data as any;
      return [
        result,
        ...(result.metadata?.results ?? []).map((child: any) => ({
          name: child.tool,
          output: child.output ?? child.error,
          is_error: !!child.error,
          metadata: child.metadata,
        })),
      ];
    });
}
async function tabs(proof: Proof) {
  return (await tool(proof.thread, "tabs_list")).tabs as any[];
}
async function value(proof: Proof, path: string, script: string) {
  const tab = (await tabs(proof)).find((t) => new URL(t.url).pathname === path);
  expect(tab, "Expected tab for " + path).toBeTruthy();
  return (await tool(proof.thread, "evaluate", { tabID: tab.id, script }))
    .value;
}
const has = (p: Proof, ...names: string[]) =>
  names.forEach((name) => expect(p.calls).toContain("browser_" + name));
const text = (p: Proof, pattern: RegExp) => expect(p.final).toMatch(pattern);
const cases: Case[] = [
  {
    id: "01_read_and_summarize",
    prompts: (b) => [
      `Please look at ${b}/catalog in the browser. What products are for sale, how much do they cost, and when is shipping free?`,
    ],
    check: async (p) => {
      text(p, /Orchid/i);
      text(p, /18/);
      text(p, /Cedar/i);
      text(p, /42/);
      text(p, /Amber/i);
      text(p, /24/);
      text(p, /50/);
      expect((await tabs(p)).length).toBeGreaterThan(0);
    },
  },
  {
    id: "02_filter_and_calculate",
    prompts: (b) => [
      `At ${b}/catalog, show me just the kitchen products. What's the total price for one of each? Leave that filter selected.`,
    ],
    check: async (p) => {
      text(p, /42/);
      expect(
        await value(p, "/catalog", "document.getElementById('category').value"),
      ).toBe("Kitchen");
      expect(
        await value(
          p,
          "/catalog",
          "document.querySelectorAll('#products tr').length",
        ),
      ).toBe(2);
    },
  },
  {
    id: "03_book_a_table",
    tui: true,
    prompts: (b) => [
      `Book a table using ${b}/booking for Alex Rivera, alex@example.test, four people on October 12, 2026. Outdoor seating, please, and add 'One vegetarian guest' to the notes. Submit it and tell me if it was confirmed.`,
    ],
    check: async (p) => {
      expect(await value(p, "/booking", "window.saved")).toEqual({
        guest: "Alex Rivera",
        email: "alex@example.test",
        party: "4",
        date: "2026-10-12",
        notes: "One vegetarian guest",
        outdoor: true,
      });
      expect(await value(p, "/booking", "window.submissions")).toBe(1);
      text(p, /confirm/i);
    },
  },
  {
    id: "04_unicode_booking",
    prompts: (b) => [
      `ブラウザで ${b}/booking を開いて、名前は「山田 花子」、メールは hanako@example.test、人数は2名、日付は2026年10月13日で予約してください。備考は「窓際を希望 🌸」、屋外席は希望しません。予約結果も教えてください。`,
    ],
    check: async (p) => {
      expect(await value(p, "/booking", "window.saved")).toEqual({
        guest: "山田 花子",
        email: "hanako@example.test",
        party: "2",
        date: "2026-10-13",
        notes: "窓際を希望 🌸",
        outdoor: false,
      });
      expect(await value(p, "/booking", "window.submissions")).toBe(1);
    },
  },
  {
    id: "05_correct_a_draft",
    prompts: (b) => [
      `Open ${b}/booking and fill a reservation for Sam Lee, sam@example.test, two guests on October 14, 2026. Leave it as a draft; don't submit yet.`,
      `Actually make that six guests, request outdoor seating, and add 'Birthday dinner'. Now submit the reservation.`,
    ],
    check: async (p) => {
      expect(await value(p, "/booking", "window.saved")).toEqual({
        guest: "Sam Lee",
        email: "sam@example.test",
        party: "6",
        date: "2026-10-14",
        notes: "Birthday dinner",
        outdoor: true,
      });
      expect(await value(p, "/booking", "window.submissions")).toBe(1);
    },
  },
  {
    id: "06_keyboard_search",
    prompts: (b) => [
      `Find the book about forests at ${b}/search. Type forest into the search field and press Enter. Tell me the author and shelf.`,
    ],
    check: async (p) => {
      text(p, /Mira Lane/);
      text(p, /C7/);
      expect(
        await value(p, "/search", "document.getElementById('query').value"),
      ).toBe("forest");
      has(p, "press");
    },
  },
  {
    id: "07_wait_for_delayed_result",
    prompts: (b) => [
      `Open ${b}/async, prepare the export, wait until it is ready, then get its export code. What is the code?`,
    ],
    check: async (p) => {
      text(p, /FIR-629/);
      expect(
        await value(p, "/async", "document.getElementById('code').textContent"),
      ).toContain("FIR-629");
    },
  },
  {
    id: "08_embedded_cross_origin_form",
    prompts: (b) => [
      `In the delivery form embedded at ${b}/embedded, set the recipient to Jamie Park and the delivery instruction to 'Leave with concierge', then save it.`,
    ],
    check: async (p) => {
      const tab = (await tabs(p))[0];
      const frames = (await tool(p.thread, "frames", { tabID: tab.id })).frames;
      const frame = frames.find(
        (f: any) => new URL(f.url).pathname === "/frame",
      );
      expect(
        (
          await tool(p.thread, "evaluate", {
            tabID: tab.id,
            frameID: frame.id,
            script: "window.saved",
          })
        ).value,
      ).toEqual({
        recipient: "Jamie Park",
        instruction: "Leave with concierge",
      });
    },
  },
  {
    id: "09_compare_two_tabs",
    prompts: (b) => [
      `Compare the total price of one Orchid mug delivered to me from ${b}/catalog and ${b}/compare. Open them in separate browser tabs, tell me which is cheaper including shipping, and keep both tabs open.`,
    ],
    check: async (p) => {
      const list = await tabs(p);
      expect(list.some((t) => /\/(catalog|shipping)$/.test(t.url))).toBe(true);
      const opened = p.results
        .filter((r) => r.name === "browser_tabs_open")
        .map((r) => r.metadata?.result?.id)
        .filter(Boolean);
      expect(opened.length).toBeGreaterThanOrEqual(2);
      expect(opened.every((id) => list.some((tab) => tab.id === id))).toBe(
        true,
      );
      expect(list.some((t) => t.url.endsWith("/compare"))).toBe(true);
      text(p, /24/);
      text(p, /same|equal|tie/i);
    },
  },
  {
    id: "10_history_and_followup_close",
    prompts: (b) => [
      `Open ${b}/catalog, follow Shipping details, then use Back to return to the shop and Forward to return to the policy. Reload the policy and tell me how long express delivery takes.`,
      `Open the shop in a second tab. Close only the shipping-policy tab and leave the shop open.`,
    ],
    check: async (p) => {
      const list = await tabs(p);
      expect(list).toHaveLength(1);
      expect(list[0].url).toContain("/catalog");
      has(p, "back", "forward", "reload", "tabs_close");
    },
  },
  {
    id: "11_popup",
    prompts: (b) => [
      `At ${b}/popup, click Open return policy. Read the new tab and tell me the return window.`,
    ],
    check: async (p) => {
      text(p, /21/);
      expect((await tabs(p)).some((t) => t.url.endsWith("/shipping"))).toBe(
        true,
      );
      has(p, "click");
    },
  },
  {
    id: "12_confirmation_dialogs",
    prompts: (b) => [
      `At ${b}/dialogs, try deleting the draft but cancel the confirmation so the draft stays. Tell me whether it is still there.`,
      `Now delete the draft and accept the confirmation.`,
    ],
    check: async (p) => {
      expect(await value(p, "/dialogs", "window.deleted")).toBe(true);
      has(p, "dialog");
    },
  },
  {
    id: "13_prompt_dialog",
    prompts: (b) => [
      `Rename the draft at ${b}/dialogs to Autumn tasting by using the Rename draft button and its popup prompt.`,
    ],
    check: async (p) => {
      expect(
        await value(
          p,
          "/dialogs",
          "document.getElementById('status').textContent",
        ),
      ).toBe("Draft: Autumn tasting");
      has(p, "dialog");
    },
  },
  {
    id: "14_visual_screenshot",
    tui: true,
    prompts: (b) => [
      `Open ${b}/visual and take a screenshot. From the image, what color is the large card, what word is printed on it, and what number is underneath?`,
    ],
    check: async (p) => {
      text(p, /purple|violet/i);
      text(p, /ORCHID/i);
      text(p, /7/);
      has(p, "screenshot");
      expect(p.results.some((r) => r.metadata?.attachments?.length)).toBe(true);
    },
  },
  {
    id: "15_upload_csv",
    prompts: (b) => [
      `Please import shipping.csv from this project into the upload field at ${b}/import. Confirm how many data rows were imported and name the recipients.`,
    ],
    check: async (p) => {
      expect(await value(p, "/import", "window.imported")).toBe(
        "recipient,city\nAvery,Osaka\nMorgan,Kyoto\n",
      );
      text(p, /2|two/i);
      text(p, /Avery/);
      text(p, /Morgan/);
      has(p, "files_upload");
    },
  },
  {
    id: "16_drop_file",
    prompts: (b) => [
      `Drag shipping.csv from this project onto the Drop CSV here area at ${b}/import, rather than using the file-picker field. Tell me whether the import succeeded.`,
    ],
    check: async (p) => {
      expect(await value(p, "/import", "window.imported")).toContain(
        "Morgan,Kyoto",
      );
      has(p, "files_drop");
      expect(await value(p, "/import", "window.dropTrusted")).toBe(true);
    },
  },
  {
    id: "17_download_report",
    prompts: (b) => [
      `Download the stock report from ${b}/download into this project. Tell me where you saved it and how many Orchid mugs are in stock.`,
    ],
    check: async (p) => {
      has(p, "files_get");
      text(p, /17/);
      const files = p.results.flatMap((r) => r.metadata?.result?.files ?? []);
      const file = files.find((f: any) => f.name === "stock.csv" && f.path);
      expect(file).toBeTruthy();
      expect(await readFile(file.path, "utf8")).toContain("Orchid mug,17");
      expect(file.workspaceRelativePath).toBe(
        "./" + relative(workspace, file.path),
      );
      const localPath = relative(workspace, file.path).replace(
        /[.*+?^${}()|[\]\\]/g,
        "\\$&",
      );
      expect(
        p.final.includes(file.path) ||
          new RegExp(
            "(^|[\\s`(\\[])(?:\\./)?" + localPath + "(?=$|[\\s`)\\]])",
          ).test(p.final),
        "The answer must identify the actual saved file using its absolute or workspace-relative path",
      ).toBe(true);
    },
  },
  {
    id: "18_console_diagnosis",
    tui: true,
    prompts: (b) => [
      `Save settings is broken at ${b}/broken. Reproduce it in the browser and inspect the console. Quote the error and tell me the HTTP status of the failed save. Don't change the application.`,
    ],
    check: async (p) => {
      text(p, /SAVE_REJECTED/);
      text(p, /422/);
      has(p, "console");
      expect(await value(p, "/broken", "window.lastStatus")).toBe(422);
    },
  },
  {
    id: "19_network_response",
    prompts: (b) => [
      `Open ${b}/broken and click Save settings once. Look at the failed request's response body: what exact backend error code and request ID did it return, and when does maintenance end?`,
    ],
    check: async (p) => {
      text(p, /WAREHOUSE_LOCKED/);
      text(p, /lab-req-742/);
      text(p, /18:45/);
      has(p, "network_get");
    },
  },
  {
    id: "20_performance_trace",
    prompts: (b) => [
      `At ${b}/slow, record a short browser performance trace while clicking Calculate report, stop the recording, and summarize what the trace shows. Save the trace in this project.`,
    ],
    check: async (p) => {
      has(p, "trace_start", "trace_stop", "trace_analyze");
      const analysis = p.results.find((r) => r.name === "browser_trace_analyze")
        ?.metadata?.result;
      expect(
        analysis?.metrics.find((metric: any) => metric.name === "longTasks")
          ?.value,
      ).toBeGreaterThanOrEqual(1);
      expect(
        analysis?.events.find((event: any) => event.name === "FunctionCall")
          ?.maxMs,
      ).toBeGreaterThan(250);
      expect(
        p.results.some((r) =>
          r.metadata?.result?.files?.some((f: any) => f.path),
        ),
      ).toBe(true);
      expect(
        await value(
          p,
          "/slow",
          "document.getElementById('result').textContent",
        ),
      ).toContain("calculated");
    },
  },
  {
    id: "21_accessibility_audit",
    prompts: (b) => [
      `Run an accessibility audit of ${b}/audit in the browser. Give me the score and two concrete issues, and keep the audit report in the project.`,
    ],
    check: async (p) => {
      has(p, "lighthouse");
      text(p, /button|name|label|contrast|alt/i);
      const audit = p.results.find((r) => r.metadata?.result?.scores);
      expect(audit?.metadata?.result?.files?.length).toBeGreaterThan(0);
    },
  },
  {
    id: "22_drag_card",
    prompts: (b) => [
      `Move 'Ship browser tests' from To do into the Done column at ${b}/board by dragging the card. Verify that it moved.`,
    ],
    check: async (p) => {
      expect(
        await value(
          p,
          "/board",
          "document.getElementById('card').parentElement.id",
        ),
      ).toBe("done");
      has(p, "drag");
      expect(await value(p, "/board", "window.dropTrusted")).toBe(true);
    },
  },
  {
    id: "23_hover_tooltip",
    prompts: (b) => [
      `At ${b}/hover, hover over Shipping information and read the tooltip. What is the express cutoff time?`,
    ],
    check: async (p) => {
      text(p, /16:30/);
      has(p, "hover");
    },
  },
  {
    id: "24_scroll_and_finish",
    tui: true,
    prompts: (b) => [
      `Scroll to the bottom of ${b}/scroll and finish the checklist. Tell me its completion code, then take a screenshot of the completed area.`,
    ],
    check: async (p) => {
      text(p, /PINE-804/);
      expect(
        await value(
          p,
          "/scroll",
          "document.getElementById('result').textContent",
        ),
      ).toContain("PINE-804");
      has(p, "screenshot");
    },
  },
];

test.beforeAll(async () => {
  test.setTimeout(120000);
  root = await realpath(
    await mkdtemp(join(tmpdir(), "haarness-browser-language-")),
  );
  // A concurrent development rebuild must not replace the fixture daemon on reload.
  binary = join(root, "haarness");
  await copyFile(sourceBinary, binary);
  workspace = join(root, "project");
  env = Object.fromEntries(
    Object.entries(process.env).filter(
      (x): x is [string, string] => x[1] !== undefined,
    ),
  );
  Object.assign(env, {
    HAARNESS_BIN: binary,
    HAARNESS_HOME: join(root, "home"),
    HAARNESS_DATA_DIR: join(root, "state"),
    HAARNESS_DESKTOP_DATA: join(root, "desktop"),
    HAARNESS_DESKTOP_TEST: "1",
  });
  delete env.ELECTRON_RUN_AS_NODE;
  if (!env.OPENAI_API_KEY)
    env.OPENAI_API_KEY = JSON.parse(
      await readFile(
        join(homedir(), ".local/share/haarness/auth.json"),
        "utf8",
      ),
    ).openai.key;
  await mkdir(join(workspace, ".haarness"), { recursive: true });
  await mkdir(evidence, { recursive: true });
  await writeFile(
    join(workspace, ".haarness/config.toml"),
    '[defaults]\nprovider="openai"\nmodel="' +
      model +
      '"\n\n[permissions]\nbrowser="allow"\nbash="deny"\n',
  );
  await writeFile(
    join(workspace, "shipping.csv"),
    "recipient,city\nAvery,Osaka\nMorgan,Kyoto\n",
  );
  for (const args of [
    ["init", "-b", "main"],
    ["add", "."],
    [
      "-c",
      "user.name=Browser QA",
      "-c",
      "user.email=qa@example.test",
      "commit",
      "-m",
      "Browser language fixtures",
    ],
  ])
    execFileSync("git", args, { cwd: workspace, stdio: "ignore" });
  site = createServer(browserLab);
  await new Promise<void>((r) => site.listen(0, "127.0.0.1", r));
  siteURL = "http://127.0.0.1:" + (site.address() as any).port;
  app = await electron.launch({ args: ["."], env, timeout: 120000 });
  // Leave native page dialogs for Haarness; Playwright otherwise auto-dismisses them.
  app.context().on("dialog", () => {});
  page = await app.firstWindow();
  await app.evaluate(({ BrowserWindow }) => {
    const w = BrowserWindow.getAllWindows()[0];
    w.setContentSize(1500, 980);
    w.show();
  });
  await app.evaluate(({ dialog }, dir) => {
    dialog.showOpenDialog = async () => ({ canceled: false, filePaths: [dir] });
  }, workspace);
  await page
    .getByRole("region", { name: "Project home" })
    .getByRole("button", { name: "Open project", exact: true })
    .click();
  await expect(page.locator(".application")).toHaveAttribute(
    "data-project-ready",
    "true",
  );
  endpoint = JSON.parse(
    execFileSync(
      binary,
      ["--workspace", workspace, "--trust-project", "desktop"],
      { env, input: '{"action":"connect"}', encoding: "utf8", timeout: 30000 },
    ),
  );
  client = new Haarness({ baseUrl: endpoint.url, token: endpoint.token });
  connection = "local:" + workspace;
});
test.afterAll(async () => {
  await app?.close().catch(() => {});
  client?.close();
  await new Promise<void>((r) => site?.close(() => r()));
  if (workspace)
    try {
      execFileSync(
        binary,
        ["--workspace", workspace, "--trust-project", "serve", "--stop"],
        { env, stdio: "ignore", timeout: 30000 },
      );
    } catch {}
});
async function activate(thread: Thread) {
  await page.evaluate(
    async ({ connection, thread }) => {
      await window.haarness.storage("active-server", "local");
      await window.haarness.storage("server-project:local", connection);
      await window.haarness.storage("active-project", connection);
      await window.haarness.storage("session:" + connection, thread);
    },
    { connection, thread: thread.id },
  );
  await page.reload();
  await expect
    .poll(async () => {
      try {
        return attachedTabs(await tool(thread, "tabs_list"));
      } catch {
        return -1;
      }
    })
    .toBe(0);
}
for (const scenario of cases)
  test(scenario.id, async ({}, testInfo) => {
    test.setTimeout(480000);
    const folder = join(
      evidence,
      "repeat-" + (testInfo.repeatEachIndex + 1),
      scenario.id,
    );
    await mkdir(folder, { recursive: true });
    const thread = await client.createThread({
      title: scenario.id,
      provider: "openai",
      model,
    });
    await activate(thread);
    const prompts = scenario.prompts(siteURL),
      turns: string[] = [],
      timings: number[] = [];
    const tui = scenario.tui
      ? "haarness-browser-language-" + crypto.randomUUID().slice(0, 8)
      : undefined;
    let proof: Proof = {
      items: [],
      final: "",
      calls: [],
      results: [],
      thread,
      folder,
    };
    let failure = "";
    try {
      if (tui) {
        execFileSync(
          "tmux",
          [
            "new-session",
            "-d",
            "-s",
            tui,
            "-x",
            "150",
            "-y",
            "45",
            "env",
            "HAARNESS_SERVER_TOKEN=" + endpoint.token,
            "HAARNESS_HOME=" + join(root, "tui"),
            binary,
            "attach",
            endpoint.url,
            "--thread",
            thread.id,
          ],
          { env },
        );
        await expect
          .poll(() =>
            execFileSync("tmux", ["capture-pane", "-p", "-t", tui], {
              encoding: "utf8",
            }),
          )
          .toContain(scenario.id);
      }
      for (const prompt of prompts) {
        const old = (await client.listItems(thread.id)).filter(
          (i) => i.data.type === "user_message",
        ).length;
        const start = Date.now();
        if (tui) {
          execFileSync("tmux", ["send-keys", "-t", tui, "-l", prompt]);
          execFileSync("tmux", ["send-keys", "-t", tui, "Enter"]);
        } else {
          await page
            .getByRole("textbox", { name: "Message", exact: true })
            .fill(prompt);
          await page
            .getByRole("button", { name: "Send message", exact: true })
            .click();
        }
        let id: string | undefined;
        await expect
          .poll(async () => {
            const users = (await client.listItems(thread.id)).filter(
              (i) => i.data.type === "user_message",
            );
            id = users[old]?.turnId ?? undefined;
            return id;
          })
          .toBeTruthy();
        turns.push(id!);
        await expect
          .poll(
            async () => {
              const t = await client.getTurn(id!);
              return t.status;
            },
            { timeout: 180000, intervals: [500, 1000] },
          )
          .toBe("completed");
        timings.push(Date.now() - start);
        // Verify the first half of two-turn requests before allowing the follow-up.
        if (turns.length === 1 && scenario.id === "05_correct_a_draft")
          expect(await value(proof, "/booking", "window.submissions")).toBe(0);
        if (turns.length === 1 && scenario.id === "12_confirmation_dialogs")
          expect(await value(proof, "/dialogs", "window.deleted")).toBe(false);
      }
      proof.items = (await client.listItems(thread.id)).filter(
        (i) => i.turnId && turns.includes(i.turnId),
      );
      proof.calls = proof.items
        .filter((i) => i.data.type === "tool_call")
        .map((i) => (i.data as any).name);
      proof.results = recordedResults(proof.items);
      proof.calls.push(
        ...proof.results
          .filter((r) => r.name?.startsWith("browser_"))
          .map((r) => r.name)
          .filter((name) => !proof.calls.includes(name)),
      );
      proof.final = proof.items
        .filter((i) => i.data.type === "assistant_message")
        .map((i) => (i.data as any).text)
        .join("\n");
      expect(proof.calls.some((name) => name.startsWith("browser_"))).toBe(
        true,
      );
      await scenario.check(proof);
    } catch (error) {
      failure = String(error);
      throw error;
    } finally {
      proof.items = (await client.listItems(thread.id).catch(() => [])).filter(
        (i) => i.turnId && turns.includes(i.turnId),
      );
      const errors = recordedResults(proof.items)
        .filter((item) => item.is_error)
        .map((item) => ({ name: item.name, output: item.output }));
      const summary = {
        id: scenario.id,
        model,
        client: tui ? "TUI" : "Desktop",
        prompts,
        turns,
        timings,
        passed: !failure,
        failure,
        toolCalls: proof.items
          .filter((i) => i.data.type === "tool_call")
          .map((i) => (i.data as any).name),
        toolErrors: errors,
        answers: proof.items
          .filter((i) => i.data.type === "assistant_message")
          .map((i) => (i.data as any).text),
        fixture: root,
      };
      await writeFile(
        join(folder, "result.json"),
        JSON.stringify(summary, null, 2),
      );
      await writeFile(
        join(folder, "items.json"),
        JSON.stringify(proof.items, null, 2),
      );
      if (tui) {
        try {
          await writeFile(
            join(folder, "tui.txt"),
            execFileSync(
              "tmux",
              ["capture-pane", "-p", "-S", "-500", "-t", tui],
              { encoding: "utf8" },
            ),
          );
          execFileSync("tmux", ["kill-session", "-t", tui]);
        } catch {}
      }
      for (const id of turns) {
        const turn = await client.getTurn(id).catch(() => undefined);
        if (turn && !["completed", "failed", "cancelled"].includes(turn.status))
          await client.interruptTurn(id).catch(() => {});
      }
      let number = 0;
      for (const item of proof.items)
        if (item.data.type === "tool_result")
          for (const path of (item.data as any).metadata?.attachments ?? [])
            await copyFile(
              path,
              join(folder, "screenshot-" + ++number + ".png"),
            ).catch(() => {});
    }
  });

import test, { mock } from "node:test";
import assert from "node:assert/strict";
import { createPage, lastRef } from "../src/page";
import { Browser } from "../src/rpc";
import { BrowserCodeError, browserFailure } from "../src/errors";
import type { PageExtension } from "../src/types";
import { fakeCdp, fakeSurface, frameTree, memoryFiles, pathInput, tabID } from "./fake";

const signal = () => new AbortController().signal;
const ax = {
  nodes: [
    { nodeId: "1", ignored: false, role: { type: "role", value: "RootWebArea" }, name: { type: "computedString", value: "Shop" }, childIds: ["2", "3", "4"], backendDOMNodeId: 1 },
    { nodeId: "2", ignored: false, role: { type: "role", value: "button" }, name: { type: "computedString", value: "Buy now" }, backendDOMNodeId: 10 },
    { nodeId: "3", ignored: false, role: { type: "role", value: "textbox" }, name: { type: "computedString", value: "Name" }, value: { type: "string", value: "Ada" }, backendDOMNodeId: 11 },
    { nodeId: "4", ignored: false, role: { type: "role", value: "textbox" }, name: { type: "computedString", value: "Password" }, value: { type: "string", value: "secret" }, backendDOMNodeId: 12 },
  ],
};

function page(options: { surface?: Parameters<typeof fakeSurface>[0]; tree?: ReturnType<typeof frameTree>; extension?: PageExtension<Browser.File>; refSeed?: number } = {}) {
  const tree = options.tree ?? frameTree("https://shop.test/cart", "https://shop.test");
  const cdp = fakeCdp({
    "Page.getFrameTree": () => tree,
    "Accessibility.getFullAXTree": () => ax,
    "DOM.describeNode": ({ backendNodeId }) => ({ node: { attributes: backendNodeId === 12 ? ["type", "password"] : ["type", "text"] } }),
    "DOM.resolveNode": ({ backendNodeId }) => ({ object: { objectId: `node-${backendNodeId}` } }),
    "Runtime.callFunctionOn": ({ functionDeclaration }) => ({
      result: {
        value: String(functionDeclaration).includes("getBoundingClientRect")
          ? { x: 10, y: 20, width: 100, height: 40 }
          : String(functionDeclaration).includes("this.readOnly")
            ? "text"
            : undefined,
      },
    }),
    "Runtime.evaluate": () => ({ result: { value: { text: "Thanks", truncated: false } } }),
    "Page.getLayoutMetrics": () => ({ cssVisualViewport: { pageX: 0, pageY: 0, clientWidth: 800, clientHeight: 600 }, cssContentSize: { x: 0, y: 0, width: 800, height: 2000 } }),
    "Page.captureScreenshot": () => ({ data: "AQID" }),
  });
  const surface = fakeSurface(options.surface);
  surface.setURL(tree.frameTree.frame.url);
  const files = memoryFiles();
  const input = pathInput();
  const published: number[] = [];
  const created = createPage<Browser.File>(surface, cdp, {
    id: tabID,
    publish: () => published.push(created.state().generation),
    files,
    input,
    refSeed: options.refSeed,
    extension: options.extension,
  });
  return { page: created, cdp, surface, files, input, published };
}

test("ready enables the DevTools domains in Desktop's order, after the host's first load", async () => {
  const cdp = fakeCdp();
  const order: string[] = [];
  const created = createPage(fakeSurface(), cdp, {
    id: tabID,
    publish() {},
    files: memoryFiles(),
    input: pathInput(),
    start: async () => void order.push("start:" + cdp.methods().length),
  });
  await created.ready;
  assert.deepEqual(order, ["start:0"]);
  assert.deepEqual(cdp.methods(), [
    "Runtime.enable",
    "Page.enable",
    "DOM.enable",
    "Target.setAutoAttach",
    "Log.enable",
    "Network.enable",
  ]);
});

test("a navigation the browser reports while the first load starts moves the generation", async () => {
  const cdp = fakeCdp();
  const order: string[] = [];
  const created = createPage(fakeSurface(), cdp, {
    id: tabID,
    publish() {},
    generation: 4,
    files: memoryFiles(),
    input: pathInput(),
    start: async (page) => {
      // Electron emits did-start-navigation inside loadURL, before createPage returns.
      page.navigationStarted();
      order.push("start:" + page.state().generation);
    },
  });
  assert.equal(created.state().generation, 5);
  await created.ready;
  assert.deepEqual(order, ["start:5"]);
});

test("snapshots mint process-wide refs from the seed, and a navigation expires them", async () => {
  const { page: tab, published } = page({ refSeed: lastRef() + 1_000 });
  await tab.ready;
  const seed = lastRef();
  const snapshot = await tab.execute({ action: { type: "snapshot", tabID }, files: [] }, signal());
  const content = (snapshot.value as { content: string }).content;
  assert.match(content, new RegExp(`@e${seed + 1} \\[button\\] "Buy now"`));
  assert.match(content, /\[textbox\] "Name" value="Ada"/);
  assert.match(content, /\[textbox\] "Password" value="\[redacted\]"/);
  assert.equal(lastRef(), seed + 3);
  // A second page continues the same counter.
  const other = page();
  await other.page.ready;
  const next = await other.page.execute({ action: { type: "snapshot", tabID }, files: [] }, signal());
  assert.match((next.value as { content: string }).content, new RegExp(`@e${seed + 4} `));
  tab.navigationStarted();
  assert.deepEqual(published, [1]);
  await assert.rejects(
    tab.execute({ action: { type: "hover", tabID, ref: Browser.Ref.make(`e${seed + 1}`) }, files: [] }, signal()),
    /Element ref is stale/,
  );
  await assert.rejects(
    tab.execute({ action: { type: "frames", tabID }, files: [], generation: 0 }, signal()),
    /The document changed before this operation ran/,
  );
});

test("Desktop targets list document URLs, and captures every frame document", async () => {
  const { page: tab, cdp } = page({
    tree: frameTree("https://shop.test/cart", "https://shop.test", [
      { id: "ad", url: "https://ads.test/frame", securityOrigin: "https://ads.test" },
    ]),
  });
  await tab.ready;
  assert.deepEqual(await tab.inspect({ type: "navigate", tabID, url: "example.com/a" }), {
    resources: ["https://example.com/a"],
    key: JSON.stringify([0, 0, []]),
  });
  assert.deepEqual((await tab.inspect({ type: "screenshot", tabID })).resources, [
    "https://ads.test/frame",
    "https://shop.test/cart",
  ]);
  assert.deepEqual(await tab.inspect({ type: "back", tabID }), {
    resources: ["https://back.test/"],
    key: JSON.stringify([0, 0, 2, "https://back.test/"]),
  });
  // A frame navigation changes the key, so a pending approval no longer matches.
  cdp.emit("Page.frameNavigated", { frame: { id: "ad", parentId: "main", url: "https://ads.test/next", securityOrigin: "https://ads.test" } });
  assert.equal((await tab.inspect({ type: "evaluate", tabID, script: "1" })).key, JSON.stringify([0, 1, []]));
});

test("Chrome targets list security origins, the www twin, and the top document for captures", async () => {
  const { page: tab, files } = page({
    surface: { resourceOrigins: "securityOrigin", gateTopDocumentOnly: true, wwwTwin: true },
    tree: frameTree("https://shop.test/cart", "https://shop.test", [
      { id: "blank", url: "about:blank", securityOrigin: "https://shop.test" },
      { id: "sandbox", url: "https://evil.test/x", securityOrigin: "null" },
    ]),
  });
  await tab.ready;
  assert.deepEqual((await tab.inspect({ type: "navigate", tabID, url: "https://example.com/a?b" })).resources, [
    "https://example.com",
    "https://www.example.com",
  ]);
  assert.deepEqual((await tab.inspect({ type: "navigate", tabID, url: "about:blank" })).resources, ["about:blank"]);
  assert.deepEqual((await tab.inspect({ type: "screenshot", tabID })).resources, ["https://shop.test"]);
  assert.deepEqual((await tab.inspect({ type: "evaluate", tabID, script: "1", frameID: "blank" })).resources, ["https://shop.test"]);
  assert.deepEqual((await tab.inspect({ type: "evaluate", tabID, script: "1", frameID: "sandbox" })).resources, ["https://evil.test"]);
  assert.deepEqual((await tab.inspect({ type: "console", tabID })).resources, ["https://shop.test"]);
  const shot = await tab.execute(
    { action: { type: "screenshot", tabID }, files: [], target: { resources: ["https://shop.test"], key: JSON.stringify([0, 0, []]) } },
    signal(),
  );
  assert.deepEqual([...shot.files[0].data], [1, 2, 3]);
  assert.deepEqual(files.get(shot.files[0].id).resources, ["https://shop.test"]);
});

test("a tool the browser lacks fails as unsupported before it is inspected", async () => {
  const { page: tab, cdp } = page({ surface: { supports: (method) => method !== "evaluate" } });
  await tab.ready;
  const before = cdp.sent.length;
  const error = await tab
    .execute({ action: { type: "evaluate", tabID, script: "1" }, files: [], inspect: true }, signal())
    .catch((error: unknown) => error);
  assert.ok(error instanceof BrowserCodeError);
  assert.equal(cdp.sent.length, before);
  assert.deepEqual(browserFailure({ type: "evaluate", tabID, script: "1" }, error), {
    type: "failure",
    code: "unsupported",
    message: "This browser tool is not available in this browser.",
  });
});

test("fill selects a field's text with the host's chord or an editing command", async () => {
  for (const [selectAll, expected] of [
    [{ keys: "Meta+A" }, { type: "keyDown", key: "A", windowsVirtualKeyCode: 65, modifiers: 4 }],
    [{ commands: ["selectAll"] }, { type: "rawKeyDown", key: "a", code: "KeyA", windowsVirtualKeyCode: 65, commands: ["selectAll"] }],
  ] as const) {
    const { page: tab, cdp, surface } = page({ surface: { selectAll, repaint: async () => void surface.calls.push("repaint") } });
    await tab.ready;
    const snapshot = await tab.execute({ action: { type: "snapshot", tabID }, files: [] }, signal());
    const ref = /@(e\d+) \[textbox\] "Name"/.exec((snapshot.value as { content: string }).content)![1];
    await tab.execute({ action: { type: "fill", tabID, ref: Browser.Ref.make(ref), text: "Grace" }, files: [] }, signal());
    const keys = cdp.sent.filter((entry) => entry.method === "Input.dispatchKeyEvent").map((entry) => entry.params);
    assert.deepEqual(keys[0], expected);
    assert.equal(keys[2].key, "Backspace");
    assert.deepEqual(cdp.sent.at(-1), { method: "Input.insertText", params: { text: "Grace" }, sessionID: undefined });
    assert.ok(surface.calls.includes("repaint"));
  }
});

test("uploads go to the element as the paths the host staged", async () => {
  const { page: tab, cdp, input } = page();
  await tab.ready;
  const snapshot = await tab.execute({ action: { type: "snapshot", tabID }, files: [] }, signal());
  const ref = Browser.Ref.make(/@(e\d+) \[button\]/.exec((snapshot.value as { content: string }).content)![1]);
  const staged = { id: Browser.FileID.make("file_00000000-0000-4000-8000-000000000009"), name: "cv.pdf", mime: "application/pdf", bytes: 4, path: "/uploads/1/cv.pdf" };
  await tab.execute({ action: { type: "files.upload", tabID, ref, paths: ["cv.pdf"] }, files: [staged] }, signal());
  assert.deepEqual(input.staged, [{ files: [staged], sources: ["https://shop.test/cart"] }]);
  assert.deepEqual(cdp.sent.at(-1), { method: "DOM.setFileInputFiles", params: { files: ["/uploads/1/cv.pdf"], backendNodeId: 10 }, sessionID: undefined });
  await assert.rejects(
    tab.execute({ action: { type: "files.drop", tabID, ref, paths: ["cv.pdf"] }, files: [] }, signal()),
    /Upload command has no file bytes/,
  );
});

test("a dialog that opens during a click returns the click as dispatched", async () => {
  const { page: tab, cdp } = page();
  await tab.ready;
  const snapshot = await tab.execute({ action: { type: "snapshot", tabID }, files: [] }, signal());
  const ref = Browser.Ref.make(/@(e\d+) \[button\]/.exec((snapshot.value as { content: string }).content)![1]);
  cdp.handlers["Input.dispatchMouseEvent"] = (params) => {
    if (params.type === "mousePressed")
      cdp.emit("Page.javascriptDialogOpening", { url: "https://shop.test/cart", message: "Sure?", type: "confirm", hasBrowserHandler: false });
    return {};
  };
  const result = await tab.execute({ action: { type: "click", tabID, ref }, files: [] }, signal());
  assert.deepEqual((result.value as { dialog: unknown }).dialog, { type: "confirm", message: "Sure?", defaultValue: "" });
  await assert.rejects(
    tab.execute({ action: { type: "frames", tabID }, files: [] }, signal()),
    /A JavaScript dialog is open/,
  );
  assert.deepEqual(await tab.inspect({ type: "dialog", tabID, action: "get" }), {
    resources: ["https://shop.test/cart"],
    key: "0:1:true",
  });
});

/** A page with a clickable button, and the chooser holds it has sent so far. */
async function chooserPage() {
  const made = page();
  await made.page.ready;
  const snapshot = await made.page.execute({ action: { type: "snapshot", tabID }, files: [] }, signal());
  const ref = Browser.Ref.make(/@(e\d+) \[button\]/.exec((snapshot.value as { content: string }).content)![1]);
  const holds = () =>
    made.cdp.sent
      .filter((entry) => entry.method === "Page.setInterceptFileChooserDialog")
      .map((entry) => ({ ...entry.params, ...(entry.sessionID ? { sessionID: entry.sessionID } : {}) }));
  const click = () => made.page.execute({ action: { type: "click", tabID, ref }, files: [] }, signal());
  const opened = () => made.cdp.emit("Page.fileChooserOpened", { frameId: "main", mode: "selectSingle", backendNodeId: 10 });
  return { ...made, ref, holds, click, opened };
}
const held = { enabled: true, cancel: true };
const note = (result: { value: unknown }) => (result.value as { fileChooser?: string }).fileChooser;

test("a file chooser the page opens during an action is held back, and the action says how to upload instead", async () => {
  mock.timers.enable({ apis: ["setTimeout"] });
  try {
    const { page: tab, cdp, holds, click, opened } = await chooserPage();
    // Reading the page acts on nothing: the person's choosers are left alone.
    assert.deepEqual(holds(), []);
    cdp.handlers["Input.dispatchMouseEvent"] = (params) => {
      if (params.type === "mouseReleased") {
        // The page's click handler asked for a chooser while choosers were held.
        assert.deepEqual(holds(), [held]);
        opened();
      }
      return {};
    };
    const result = await click();
    assert.match(note(result) ?? "", /^The page opened a file chooser, which Haarness kept from appearing.*browser\.files\.upload/);
    assert.equal((result.value as { dispatched: boolean }).dispatched, true);
    // A click that opens nothing says nothing, and holding continues across it without asking twice.
    cdp.handlers["Input.dispatchMouseEvent"] = () => ({});
    assert.equal(note(await click()), undefined);
    assert.deepEqual(holds(), [held]);
    // Chromium lets a page open a chooser for seconds after a click: the hold lasts as long.
    mock.timers.tick(4_000);
    assert.deepEqual(holds(), [held]);
    mock.timers.tick(1_000);
    assert.deepEqual(holds(), [held, { enabled: false }]);
    tab.dispose();
  } finally {
    mock.timers.reset();
  }
});

test("a chooser the page opens shortly after an action is held too, and the next result says so once", async () => {
  mock.timers.enable({ apis: ["setTimeout"] });
  try {
    const { page: tab, holds, click, opened } = await chooserPage();
    assert.equal(note(await click()), undefined);
    // An upload widget that first awaited a request opens its chooser 2 s later.
    mock.timers.tick(2_000);
    opened();
    assert.deepEqual(holds(), [held]);
    const next = await tab.execute({ action: { type: "frames", tabID }, files: [] }, signal());
    assert.match(note(next) ?? "", /^Shortly after your last action, the page opened a file chooser.*browser\.files\.upload/);
    const after = await tab.execute({ action: { type: "frames", tabID }, files: [] }, signal());
    assert.equal(note(after), undefined);
    tab.dispose();
  } finally {
    mock.timers.reset();
  }
});

test("the person acting in the page ends the hold at once, but input during an agent's action does not", async () => {
  mock.timers.enable({ apis: ["setTimeout"] });
  try {
    const { page: tab, cdp, holds, click } = await chooserPage();
    cdp.handlers["Input.dispatchMouseEvent"] = () => {
      // The agent's own click reaches the host as input too.
      tab.personActed();
      return {};
    };
    await click();
    assert.deepEqual(holds(), [held]);
    tab.personActed();
    assert.deepEqual(holds(), [held, { enabled: false }]);
    // The timer that would have ended it changes nothing later.
    mock.timers.tick(5_000);
    assert.deepEqual(holds(), [held, { enabled: false }]);
    tab.dispose();
  } finally {
    mock.timers.reset();
  }
});

test("a frame that attaches while choosers are held is held too", async () => {
  mock.timers.enable({ apis: ["setTimeout"] });
  try {
    const { page: tab, cdp, holds, click } = await chooserPage();
    const frame = (sessionId: string, targetId: string) =>
      cdp.emit("Target.attachedToTarget", { sessionId, targetInfo: { targetId, type: "iframe", title: "", url: "https://pay.test/", attached: true, canAccessOpener: false }, waitingForDebugger: false });
    frame("before", "frame-before");
    await click();
    frame("during", "frame-during");
    await Promise.resolve();
    assert.deepEqual(holds(), [held, { ...held, sessionID: "before" }, { ...held, sessionID: "during" }]);
    mock.timers.tick(5_000);
    assert.deepEqual(holds().slice(3), [{ enabled: false }, { enabled: false, sessionID: "before" }, { enabled: false, sessionID: "during" }]);
    // A frame that attaches after the hold ended is left alone.
    frame("after", "frame-after");
    await Promise.resolve();
    assert.equal(holds().length, 6);
    tab.dispose();
  } finally {
    mock.timers.reset();
  }
});

test("the host runs its own tools and their permission targets", async () => {
  const extension: PageExtension<Browser.File> = {
    methods: new Set<Browser.Method>(["cpu.start", "cpu.stop"]),
    inspect: (action) => (action.type === "cpu.stop" ? { resources: ["https://profiled.test/"], key: "cpu" } : undefined),
    async execute(action, context) {
      const id = await store.save("profile.cpuprofile", "application/json", new Uint8Array([123, 125]), context.sources);
      return context.result({ tab: context.state(), started: action.type }, [await context.transfer(id)]);
    },
  };
  const { page: tab, files: store } = page({ extension });
  await tab.ready;
  assert.deepEqual(await tab.inspect({ type: "cpu.stop", tabID }), { resources: ["https://profiled.test/"], key: "cpu" });
  // Without its own rule, a host tool keeps the driver's: cpu.start is a capture.
  assert.deepEqual((await tab.inspect({ type: "cpu.start", tabID })).resources, ["https://shop.test/cart"]);
  const result = await tab.execute({ action: { type: "cpu.start", tabID }, files: [] }, signal());
  assert.equal((result.value as { started: string }).started, "cpu.start");
  assert.equal(result.files[0].name, "profile.cpuprofile");
  // A tool neither the driver nor the host runs is a routing mismatch.
  await assert.rejects(
    tab.execute({ action: { type: "heap.snapshot", tabID }, files: [] }, signal()),
    /routed to a page instead of the tab manager/,
  );
});

test("a disposed page refuses commands and stops listening", async () => {
  const { page: tab, cdp, published } = page();
  await tab.ready;
  tab.dispose();
  cdp.emit("Page.javascriptDialogOpening", { url: "https://shop.test/", message: "x", type: "alert", hasBrowserHandler: false });
  assert.deepEqual(published, []);
  await assert.rejects(
    tab.execute({ action: { type: "frames", tabID }, files: [] }, signal()),
    /Browser tab was closed/,
  );
});

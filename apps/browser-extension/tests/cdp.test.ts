/**
 * The Chrome transport across an idle detach: what it sends again once
 * attached, and how a frame's old session keeps working.
 */
import assert from "node:assert/strict";
import test from "node:test";
import { createPage } from "../src/core/driver";
import type { CdpTransport, FileStore, PageSurface } from "../src/core/driver-contract";
import { tabID } from "../src/core/browser";
import { createTransport, lasting } from "../src/platform/chrome/cdp";
import { installChrome } from "./fakes/chrome";

function recording() {
  const fake = installChrome();
  const calls: [string, unknown, string | undefined][] = [];
  (fake.chrome.debugger as { sendCommand: unknown }).sendCommand = async (target: { sessionId?: string }, method: string, params: unknown) => {
    calls.push([method, params, target.sessionId]);
    return {};
  };
  return calls;
}

test("attaching again sends the tab's session settings again, and nothing a command set up for itself", async () => {
  const calls = recording();
  const transport = createTransport(7);
  const autoAttach = { autoAttach: true, waitForDebuggerOnStart: false, flatten: true };
  await transport.send("Page.enable");
  await transport.send("Runtime.enable");
  await transport.send("Network.enable", { maxPostDataSize: 20_000 });
  await transport.send("Target.setAutoAttach", autoAttach);
  await transport.send("Network.setBypassServiceWorker", { bypass: true });
  await transport.send("DOM.enable");
  await transport.send("DOM.disable");
  await transport.send("Fetch.enable", { patterns: [] });
  await transport.send("Fetch.disable");
  await transport.send("Input.dispatchMouseEvent", { type: "mouseMoved", x: 1, y: 1 });
  await transport.send("Runtime.enable", {}, "child-session");

  transport.park();
  await assert.rejects(transport.send("Runtime.evaluate", { expression: "1" }), /let go of this tab/);
  calls.length = 0;
  await transport.resume();
  assert.deepEqual(calls, [
    ["Page.enable", {}, undefined],
    ["Runtime.enable", {}, undefined],
    ["Network.enable", { maxPostDataSize: 20_000 }, undefined],
    ["Target.setAutoAttach", autoAttach, undefined],
    ["Network.setBypassServiceWorker", { bypass: true }, undefined],
  ]);
});

test("a frame's old session names its new one after attaching again", async () => {
  const calls = recording();
  const transport = createTransport(7);
  const attached = (sessionId: string) =>
    transport.receive("Target.attachedToTarget", {
      sessionId,
      targetInfo: { targetId: "frame-target", type: "iframe", title: "", url: "https://b.example/", attached: true, canAccessOpener: false },
      waitingForDebugger: false,
    });
  attached("first");
  assert.deepEqual(transport.children(), ["first"]);
  transport.park();
  assert.deepEqual(transport.children(), []);
  await transport.resume();
  attached("second");
  transport.park();
  await transport.resume();
  attached("third");
  calls.length = 0;
  // A ref the driver kept from two attaches ago still reaches the frame.
  await transport.send("DOM.resolveNode", { backendNodeId: 5 }, "first");
  await transport.send("DOM.resolveNode", { backendNodeId: 5 }, "second");
  assert.deepEqual(
    calls.map(([method, , session]) => [method, session]),
    [
      ["DOM.resolveNode", "third"],
      ["DOM.resolveNode", "third"],
    ],
  );
});

test("every setting the shared driver gives a page's session is sent again after attaching again", async () => {
  const sent: [string, string | undefined][] = [];
  const cdp: CdpTransport = {
    async send(method, _params, sessionID) {
      sent.push([method, sessionID]);
      return {} as never;
    },
    on: () => () => {},
    dispose() {},
  };
  const surface = {
    url: () => "about:blank",
    title: () => "",
    loading: () => false,
    canGoBack: () => false,
    canGoForward: () => false,
    historyEntry: (delta: -1 | 1) => ({ index: delta }),
    navigate: async () => {},
    back() {},
    forward() {},
    reload() {},
    stop() {},
    viewport: async () => ({ width: 800, height: 600 }),
    pixelRatio: async () => 1,
    readyForCapture: async () => {},
    selectAll: { commands: ["selectAll"] },
    supports: () => true,
    gateTopDocumentOnly: true,
    resourceOrigins: "securityOrigin",
  } satisfies PageSurface;
  const files = { ready: Promise.resolve() } as unknown as FileStore;
  const page = createPage(surface, cdp, {
    id: tabID("5b0f2a3c-1d4e-4f6a-8b9c-0d1e2f3a4b5c"),
    publish() {},
    files,
    input: { stage: async () => [] },
  });
  await page.ready;
  page.dispose();
  const root = sent.filter(([, session]) => session === undefined).map(([method]) => method);
  assert.ok(root.includes("Page.enable") && root.includes("Target.setAutoAttach"), root.join(", "));
  assert.deepEqual(root.filter((method) => !lasting(method)), []);
});

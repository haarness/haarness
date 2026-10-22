import { test } from "node:test";
import assert from "node:assert/strict";

// Floating layers are placed against a window and a document root.
const properties = new Map<string, string>();
Object.assign(globalThis, {
  window: { innerWidth: 1200, innerHeight: 900 },
  document: {
    documentElement: {
      dir: "ltr",
      style: {
        setProperty: (name: string, value: string) => properties.set(name, value),
        removeProperty: (name: string) => properties.delete(name),
      },
    },
  },
});
const { TOAST_SETTLE, floatingBox, floatingSpan, onNativeArea, setNativeArea, toastPlace } = await import("../src/renderer/native-area");
const rect = (left: number, width: number, top = 40, height = 28) =>
  ({ left, right: left + width, top, bottom: top + height, width, height, x: left, y: top }) as DOMRect;

test("layers use the whole window when no browser is drawn", () => {
  setNativeArea(undefined);
  assert.deepEqual(floatingBox(rect(700, 120), 360), { width: 360, left: 700 });
  assert.equal(properties.has("--native-inline-end"), false);
});

test("a picker beside the browser stays clear of it", () => {
  setNativeArea(rect(760, 440, 40, 800));
  assert.equal(properties.get("--native-inline-end"), "440px");
  // The model picker sits near the chat column's end and would run under the page.
  const box = floatingBox(rect(620, 120), 360, 260);
  assert.equal(box.width, 360);
  assert.equal(box.left + box.width <= 760 - 8, true);
  assert.equal(box.left, 760 - 8 - 360);
});

test("a narrow column shrinks the layer before giving up on it", () => {
  setNativeArea(rect(300, 900, 40, 800));
  assert.deepEqual(floatingBox(rect(40, 120), 360, 260), { width: 284, left: 8 });
  // Too narrow even for the minimum: fall back to the window and let the page freeze.
  setNativeArea(rect(200, 1000, 40, 800));
  assert.deepEqual(floatingBox(rect(40, 120), 360, 260), { width: 360, left: 40 });
});

test("anchors over or after the browser use their own side", () => {
  setNativeArea(rect(200, 500, 40, 800));
  assert.deepEqual(floatingSpan(rect(800, 100)), { start: 700, end: 1200 });
  assert.deepEqual(floatingSpan(rect(400, 100)), { start: 0, end: 1200 });
});

test("right-to-left windows measure toasts from the left edge", () => {
  (document.documentElement as { dir: string }).dir = "rtl";
  setNativeArea(rect(0, 440, 40, 800));
  assert.equal(properties.get("--native-inline-end"), "440px");
  (document.documentElement as { dir: string }).dir = "ltr";
});

// The toast column: up to 400px, 16px in from the window's inline end: [784, 1184] in a 1200px window.
const clearance = (anchors: Parameters<typeof toastPlace>[0], raised = false) => toastPlace(anchors, raised).clearance;

test("toasts rise over the message box only while it runs under them", () => {
  setNativeArea(undefined);
  // The chat fills the window: the dock's right end is under the column.
  assert.equal(clearance({ composer: rect(270, 900, 700, 180) }), 200);
  // A side panel holds the right of the window: the dock ends before the column.
  assert.equal(clearance({ composer: rect(270, 480, 700, 180) }), undefined);
  assert.equal(clearance({ composer: rect(270, 514, 700, 180) }), undefined);
  assert.equal(clearance({ composer: rect(270, 515, 700, 180) }), 200);
  // An expanded side panel hides the chat, and its dock measures nothing.
  assert.equal(clearance({ composer: rect(0, 0, 0, 0) }), undefined);
  assert.equal(clearance({ composer: rect(270, -40, 0, 0) }), undefined);
});

test("toasts clear the terminal, which spans the window, beside a side panel and under an expanded one", () => {
  setNativeArea(undefined);
  // Side panel and terminal: the terminal's top, not the message box's.
  assert.equal(clearance({ composer: rect(270, 480, 500, 140), terminal: rect(258, 942, 640, 260) }), 260);
  // The chat hidden by an expanded panel, the terminal still open.
  assert.equal(clearance({ composer: rect(0, 0, 0, 0), terminal: rect(258, 942, 640, 260) }), 260);
  // Both under the column: whichever reaches higher.
  assert.equal(clearance({ composer: rect(270, 900, 500, 140), terminal: rect(258, 942, 640, 260) }), 400);
  assert.equal(toastPlace({ terminal: rect(258, 942, 640, 260) }).terminal, true);
});

test("raised toasts drop to their corner only once the message box is well clear of them, so a dragged divider does not bounce them", () => {
  setNativeArea(undefined);
  // The box ends just short of the column [784, 1184].
  const near = rect(270, 500, 700, 180);
  assert.equal(toastPlace({ composer: near }, false).composer, false);
  assert.equal(toastPlace({ composer: near }, true).composer, true);
  assert.equal(toastPlace({ composer: rect(270, 514 - TOAST_SETTLE, 700, 180) }, true).composer, false);
  // Reaching the column raises them at once, so they never cover Send.
  assert.equal(toastPlace({ composer: rect(270, 515, 700, 180) }, false).composer, true);
});

test("beside the browser, and from the left in right-to-left windows, the column moves with the toasts", () => {
  // The browser draws over the right 440px, so toasts sit left of it, over the chat.
  setNativeArea(rect(760, 440, 40, 800));
  assert.equal(clearance({ composer: rect(270, 480, 700, 180) }), 200);
  assert.equal(clearance({ composer: rect(0, 300, 700, 180) }), undefined);
  setNativeArea(undefined);
  (document.documentElement as { dir: string }).dir = "rtl";
  // Right to left the column is [16, 416] and a side panel holds the left.
  assert.equal(clearance({ composer: rect(420, 600, 700, 180) }), undefined);
  assert.equal(clearance({ composer: rect(300, 600, 700, 180) }), 200);
  assert.equal(clearance({ composer: rect(420, 600, 700, 180), terminal: rect(0, 942, 640, 260) }), 260);
  (document.documentElement as { dir: string }).dir = "ltr";
});

test("a change in where the browser draws is announced once", () => {
  let calls = 0;
  const stop = onNativeArea(() => calls++);
  setNativeArea(undefined);
  setNativeArea(rect(760, 440, 40, 800));
  setNativeArea(rect(760, 440, 40, 800));
  setNativeArea(undefined);
  stop();
  setNativeArea(rect(760, 440, 40, 800));
  setNativeArea(undefined);
  assert.equal(calls, 2);
});

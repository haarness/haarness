import { test } from "node:test";
import assert from "node:assert/strict";

// Floating layers are placed against a window and a document root.
const properties = new Map<string, string>();
Object.assign(globalThis, {
  window: { innerWidth: 1200 },
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
const { floatingBox, floatingSpan, setNativeArea } = await import("../src/renderer/native-area");
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

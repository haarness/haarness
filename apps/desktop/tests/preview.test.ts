import test from "node:test";
import assert from "node:assert/strict";
import { Preview } from "../src/renderer/preview";
import { previewInfo } from "../src/renderer/preview-actions";

test("preview classifies HTML, markdown, SVG and images without treating source as HTML", () => {
  for (const [path, kind] of [
    ["INDEX.HTM", "html"],
    ["report.html", "html"],
    ["report#1.html", "html"],
    ["readme.md", "markdown"],
    ["chart.svg", "svg"],
    ["photo.PNG", "image"],
  ])
    assert.equal(Preview.kind(path), kind);
  assert.equal(Preview.kind("main.ts"), undefined);
});
test("public libraries use the approved npm CDN and suspicious hosts stay blocked", () => {
  const report = Preview.rewrite(
    '<script src="https://cdn.jsdelivr.net/npm/chart.js@4/dist/chart.umd.js"></script><img src="https://unpkg.com.evil.test/x"><script src="https://evil.test/x"></script>',
  );
  assert.match(
    report.html,
    /https:\/\/unpkg.com\/chart.js@4\/dist\/chart.umd.js/,
  );
  assert.equal(report.replaced.length, 1);
  assert.deepEqual(report.blocked, [
    "https://unpkg.com.evil.test/x",
    "https://evil.test/x",
  ]);
});
test("sandbox policy denies network APIs, nested frames, forms and base URLs", () => {
  const policy = Preview.policy();
  for (const item of [
    "connect-src 'none'",
    "frame-src 'none'",
    "form-action 'none'",
    "base-uri 'none'",
  ])
    assert.ok(policy.includes(item));
  const result = Preview.frame(
    '<!doctype html><script>location.href="https://evil.test"</script>',
    { id: "revision:1" },
  );
  assert.ok(result.html.includes('sandbox="allow-scripts allow-downloads"'));
  assert.ok(!result.html.includes("allow-same-origin"));
  assert.ok(result.html.includes("frame-src blob: data:"));
  assert.ok(result.html.includes("&lt;script>location.href="));
});
test("preview document policy precedes untrusted content and bridge messages are generation scoped", () => {
  const result = Preview.document(
    '<html><head><meta http-equiv="Content-Security-Policy" content="default-src *"></head></html>',
    { id: "v2" },
  );
  assert.ok(
    result.html.indexOf("default-src 'none'") <
      result.html.indexOf("default-src *"),
  );
  assert.equal(
    Preview.message({ preview: "v1", type: "ready" }, "v2"),
    undefined,
  );
  assert.deepEqual(Preview.message({ preview: "v2", type: "ready" }, "v2"), {
    type: "ready",
  });
  assert.equal(
    previewInfo({ preview: { path: "a.html", version: "../x" } }),
    undefined,
  );
  assert.equal(
    previewInfo({ preview: { path: "a.html", version: "a".repeat(64) } })?.mode,
    "preview",
  );
});
test("a button let go over the page is reported, through the outer frame, with the preview id", () => {
  // In the page and in the frame around it: whichever is under the pointer.
  const page = Preview.document("<p>page</p>", { id: "v3" }).html;
  const frame = Preview.frame("<p>page</p>", { id: "v3" }).html;
  for (const html of [page, frame]) {
    assert.match(html, /function lift\(event\) \{ if \(event\.isTrusted\) (report|up)\(\{ type: "lift" \}\); \}/);
    assert.ok(html.includes('addEventListener("pointerup", lift, true);'));
    assert.ok(html.includes('addEventListener("pointercancel", lift, true);'));
  }
  // The outer frame passes the page's report on.
  assert.ok(frame.includes('"navigation", "lift"]'));
  assert.deepEqual(Preview.message({ preview: "v3", type: "lift" }, "v3"), { type: "lift" });
  assert.equal(Preview.message({ preview: "v2", type: "lift" }, "v3"), undefined);
});

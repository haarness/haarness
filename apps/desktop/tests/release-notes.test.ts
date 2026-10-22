import test from "node:test";
import assert from "node:assert/strict";
import { releaseNotesText } from "../src/release-notes";

test("a feed's release notes become one Markdown text, and none read as none", () => {
  assert.equal(releaseNotesText("  ## Highlights\n\n- Faster  \n"), "## Highlights\n\n- Faster");
  // A full changelog: one note per version, newest first, empty ones left out.
  assert.equal(
    releaseNotesText([
      { version: "0.1.5", note: "- Two" },
      { version: "0.1.4", note: "  " },
      { version: "0.1.3", note: "- One" },
      { version: 7, note: "- Not a version" },
      null,
    ]),
    "### 0.1.5\n\n- Two\n\n### 0.1.3\n\n- One",
  );
  for (const none of [null, undefined, "", "   ", [], 42, {}]) assert.equal(releaseNotesText(none), "");
  // A whole changelog in one string is cut, not shown in full.
  const long = releaseNotesText("x".repeat(50_000));
  assert.equal(long.length, 20_000);
  assert.ok(long.endsWith("…"));
});

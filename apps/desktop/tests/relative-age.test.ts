import test from "node:test";
import assert from "node:assert/strict";
import { relativeAge } from "../src/renderer/relative-age";

test("a sidebar row's last activity reads as a narrow age, then as a date", () => {
  const now = Date.parse("2026-10-18T12:00:00Z");
  const ago = (ms: number) => new Date(now - ms).toISOString();
  const minute = 60_000;
  assert.equal(relativeAge(ago(20_000), now, "en", "now"), "now");
  assert.equal(relativeAge(ago(5 * minute), now, "en", "now"), "5m");
  assert.equal(relativeAge(ago(3 * 60 * minute + 59 * minute), now, "en", "now"), "3h");
  assert.equal(relativeAge(ago(2 * 24 * 60 * minute), now, "en", "now"), "2d");
  assert.equal(relativeAge(ago(15 * 24 * 60 * minute), now, "en", "now"), "2w");
  assert.equal(relativeAge("2026-09-03T12:00:00Z", now, "en", "now"), "Sep 3");
  assert.equal(relativeAge("2025-09-03T12:00:00Z", now, "en", "now"), "Sep 2025");
  // Units follow the reader's language; a clock ahead of the server is "now".
  assert.match(relativeAge(ago(5 * minute), now, "de", "jetzt"), /^5\s?Min/);
  assert.equal(relativeAge(new Date(now + 90_000).toISOString(), now, "en", "now"), "now");
  assert.equal(relativeAge("not a date", now, "en", "now"), "");
});

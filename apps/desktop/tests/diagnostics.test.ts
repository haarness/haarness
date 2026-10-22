import test from "node:test";
import assert from "node:assert/strict";
import { logsFileName, minutesLeft } from "../src/diagnostics";

test("the logs zip is named for the day it is saved", () => {
  assert.equal(logsFileName(new Date(2026, 0, 5, 23, 59)), "haarness-logs-2026-01-05.zip");
  assert.equal(logsFileName(new Date(2026, 9, 22, 8)), "haarness-logs-2026-10-22.zip");
});

test("Detailed logging counts whole minutes down, rounding up, and reads 0 once over", () => {
  const now = Date.parse("2026-10-22T10:00:00Z");
  assert.equal(minutesLeft("2026-10-22T10:30:00Z", now), 30);
  assert.equal(minutesLeft("2026-10-22T10:29:01Z", now), 30);
  assert.equal(minutesLeft("2026-10-22T10:00:20Z", now), 1);
  assert.equal(minutesLeft("2026-10-22T09:59:59Z", now), 0);
  assert.equal(minutesLeft(null, now), 0);
  assert.equal(minutesLeft("not a time", now), 0);
});

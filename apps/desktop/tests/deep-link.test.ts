import test from "node:test";
import assert from "node:assert/strict";
import { parseDeepLink } from "../src/deep-link";

test("a haarness link names a local folder and a chat, trimmed", () => {
  assert.deepEqual(parseDeepLink("haarness://open?workspace=/Users/me/app&session=thr_1"), {
    workspace: "/Users/me/app",
    session: "thr_1",
  });
  assert.deepEqual(
    parseDeepLink("  haarness://open?workspace=%20%2FUsers%2Fme%2Fmy%20app%20&session=%20thr_2%0A "),
    { workspace: "/Users/me/my app", session: "thr_2" },
  );
  assert.deepEqual(parseDeepLink("haarness://x?session=0199f000-0000-7000-8000-000000000001"), {
    session: "0199f000-0000-7000-8000-000000000001",
  });
  assert.deepEqual(parseDeepLink("haarness://open?workspace=C:%5CUsers%5Cme%5Capp"), {
    workspace: "C:\\Users\\me\\app",
  });
});

test("a link without a usable folder or chat opens nothing", () => {
  for (const link of [
    "",
    "not a url",
    "https://example.test/?workspace=/Users/me/app",
    "haarness://open",
    "haarness://open?workspace=%20&session=",
    // Not absolute: there is no folder it could mean.
    "haarness://open?workspace=app",
    "haarness://open?workspace=~/app",
    // Network shares, which resolving would already reach.
    "haarness://open?workspace=//server/share",
    "haarness://open?workspace=%5C%5Cserver%5Cshare",
    "haarness://open?workspace=%5C%5C%3F%5CUNC%5Cserver%5Cshare",
    "haarness://open?workspace=/%5Cserver/share",
    "haarness://open?workspace=C://server",
    "haarness://open?session=../../etc",
    "haarness://open?session=" + "x".repeat(201),
  ])
    assert.equal(parseDeepLink(link), undefined, link);
});

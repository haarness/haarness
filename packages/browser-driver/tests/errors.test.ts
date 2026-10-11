import test from "node:test";
import assert from "node:assert/strict";
import { BrowserCodeError, browserFailure, protocolError } from "../src/errors";

const navigate = { type: "navigate", tabID: "tab_00000000-0000-4000-8000-000000000001", url: "localhost:9" } as never;

test("the Haarness browser's hints keep Desktop's wording", () => {
  assert.deepEqual(browserFailure(navigate, new Error("net::ERR_CONNECTION_REFUSED")), {
    type: "failure",
    code: "navigation_failed",
    message:
      "browser_navigate failed. The browser could not connect to the site. Check its hostname/port on the connected server. localhost means that server, not the desktop. Details: net::ERR_CONNECTION_REFUSED",
  });
  assert.equal(
    protocolError("Fetch.enable", new Error("'Fetch.enable' wasn't found")).message,
    "This Chromium target does not support or allow the operation. Check Desktop/server compatibility and report it; do not retry unchanged or disable browser security. Chromium command Fetch.enable failed: 'Fetch.enable' wasn't found",
  );
});

test("Chrome's hints describe the user's own computer", () => {
  const failure = browserFailure(navigate, new Error("net::ERR_CONNECTION_REFUSED"), "chrome");
  assert.match(failure.message, /localhost means the user's computer/);
  assert.doesNotMatch(failure.message, /desktop|connected server/);
  assert.doesNotMatch(protocolError("X.y", new Error("not allowed"), "chrome").message, /Desktop/);
});

test("a coded failure keeps its code for the server to word", () => {
  assert.deepEqual(browserFailure(navigate, new BrowserCodeError("restricted-page", "Cannot control browser.tabs.list pages")), {
    type: "failure",
    code: "restricted-page",
    message: "Cannot control browser_tabs_list pages",
  });
});

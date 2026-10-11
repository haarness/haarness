import test from "node:test";
import assert from "node:assert/strict";
import { createDiagnostics, isSensitiveHeader } from "../src/diagnostics";
import { fakeCdp } from "./fake";

function capture(options?: Parameters<typeof createDiagnostics>[1]) {
  const cdp = fakeCdp({
    "Network.getResponseBody": () => ({ body: Buffer.from("héllo").toString("base64"), base64Encoded: true }),
  });
  const diagnostics = createDiagnostics(cdp, options);
  cdp.emit("Network.requestWillBeSent", {
    requestId: "1",
    loaderId: "l",
    documentURL: "https://shop.test/",
    request: {
      url: "https://shop.test/api",
      method: "POST",
      headers: { Cookie: "sid=1", "X-CSRFToken": "t", "X-Auth-Token": "a", "X-Api-Key": "k", Accept: "*/*" },
      postData: "password=hunter2",
      hasPostData: true,
    },
    timestamp: 1,
    wallTime: 1_700_000_000,
    initiator: { type: "other" },
    type: "Fetch",
  });
  cdp.emit("Network.responseReceived", {
    requestId: "1",
    loaderId: "l",
    timestamp: 2,
    type: "Fetch",
    response: { url: "https://shop.test/api", status: 200, statusText: "OK", headers: { "Content-Type": "application/json" }, mimeType: "application/json" },
    hasExtraInfo: false,
  });
  cdp.emit("Network.loadingFinished", { requestId: "1", timestamp: 3, encodedDataLength: 5 });
  const id = diagnostics.list({ type: "network.list", tabID: "" as never }).requests[0].id;
  return diagnostics.get({ type: "network.get", tabID: "" as never, id, includeBody: true });
}

test("network.get returns bodies and redacts credentials by default", async () => {
  const request = await capture();
  assert.deepEqual(request.requestBody, { state: "text", text: "password=hunter2", truncated: false });
  assert.deepEqual(request.responseBody, { state: "text", text: "héllo", truncated: false });
  const headers = Object.fromEntries(request.requestHeaders.map(({ name, value }) => [name, value]));
  assert.deepEqual(headers, { cookie: "<redacted>", "x-csrftoken": "t", "x-auth-token": "a", "x-api-key": "k", accept: "*/*" });
});

test("Chrome omits request bodies and redacts session headers", async () => {
  const request = await capture({ requestBodies: false, redactHeader: isSensitiveHeader });
  assert.deepEqual(request.requestBody, { state: "unavailable", reason: "omitted" });
  const headers = Object.fromEntries(request.requestHeaders.map(({ name, value }) => [name, value]));
  assert.deepEqual(headers, { cookie: "<redacted>", "x-csrftoken": "<redacted>", "x-auth-token": "<redacted>", "x-api-key": "<redacted>", accept: "*/*" });
  for (const name of ["x-xsrf-token", "x-csrf-token", "set-cookie2"]) assert.ok(isSensitiveHeader(name), name);
  for (const name of ["x-request-id", "content-type", "x-token-count"]) assert.ok(!isSensitiveHeader(name), name);
});

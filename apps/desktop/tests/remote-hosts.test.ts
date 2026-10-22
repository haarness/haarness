import test from "node:test";
import assert from "node:assert/strict";
import { createServer, type IncomingMessage, type ServerResponse } from "node:http";
import { once } from "node:events";
import { ApiError } from "@haarness/sdk";
import {
  cloudOrigin,
  connectHost,
  createRelay,
  hostBrowse,
  hostCatalog,
  hostError,
  hostLocationId,
  hostRuntime,
} from "../src/main/remote-hosts";

const say = (key: string) => key;
const host = "8c6f2c4e-2f6c-4c55-9a35-0d6f6d3a8a01";

async function withCloud(
  respond: (request: IncomingMessage, response: ServerResponse) => void,
  run: (url: string) => Promise<void>,
) {
  const server = createServer(respond).listen(0, "127.0.0.1");
  await once(server, "listening");
  const { port } = server.address() as { port: number };
  try {
    await run(`http://127.0.0.1:${port}`);
  } finally {
    server.closeAllConnections();
    await new Promise<void>((resolve) => server.close(() => resolve()));
  }
}
const json = (response: ServerResponse, status: number, body: unknown) => {
  response.writeHead(status, { "Content-Type": "application/json" });
  response.end(JSON.stringify(body));
};

test("Cloud addresses are HTTPS, or HTTP on this computer, and a host's gateway sits under its runtime path", () => {
  assert.equal(cloudOrigin("https://haarness.ai/"), "https://haarness.ai");
  assert.equal(cloudOrigin("http://127.0.0.1:18080/"), "http://127.0.0.1:18080");
  for (const bad of ["http://cloud.example", "https://user:pw@cloud.example", "https://cloud.example/?a=1", "ftp://cloud.example"])
    assert.throws(() => cloudOrigin(bad), /HTTPS/, bad);
  assert.equal(
    hostRuntime("https://haarness.ai/", host),
    `https://haarness.ai/api/v1/remote-hosts/${host}/runtime`,
  );
  assert.equal(hostLocationId(host, "w/1"), `host:${host}/workspaces/w%2F1`);
});

test("one sign-in serves every request, a refused one is renewed once, and a sign-out forgets it", async () => {
  const asked: (string | undefined)[] = [];
  let issued = 0;
  const relay = createRelay(async (rejected) => {
    asked.push(rejected);
    return { token: `token-${++issued}`, apiUrl: "http://127.0.0.1:1/" };
  });
  const [a, b] = await Promise.all([relay.credential(), relay.credential()]);
  assert.equal(a.token, "token-1");
  assert.equal(b.token, "token-1");
  assert.equal(a.apiUrl, "http://127.0.0.1:1");
  assert.deepEqual(await relay.auth(), { type: "bearer", token: "token-1" });
  assert.equal((await relay.credential("token-1")).token, "token-2");
  assert.deepEqual(asked, [undefined, "token-1"]);
  relay.forget();
  assert.equal((await relay.credential()).token, "token-3");
  await assert.rejects(
    createRelay(async () => ({ token: "", apiUrl: "http://127.0.0.1:1" })).credential(),
    /Sign in to Haarness/,
  );
});

test("relayed requests carry the sign-in and are sent again with a fresh one when Cloud refuses it", async () => {
  const seen: string[] = [];
  await withCloud(
    (request, response) => {
      seen.push(`${request.method} ${request.headers.authorization}`);
      let body = "";
      request.on("data", (chunk) => (body += chunk));
      request.on("end", () => {
        if (request.headers.authorization === "Bearer stale") json(response, 401, { error: { code: "unauthorized", message: "expired" } });
        else json(response, 200, { echoed: body });
      });
    },
    async (url) => {
      let issued = 0;
      const relay = createRelay(async () => ({ token: issued++ ? "fresh" : "stale", apiUrl: url }));
      const response = await relay.fetch(url + "/x", { method: "POST", body: "payload", headers: { "Content-Type": "text/plain" } });
      assert.deepEqual(await response.json(), { echoed: "payload" });
      assert.deepEqual(seen, ["POST Bearer stale", "POST Bearer fresh"]);
    },
  );
});

test("a computer's projects, folders and one of its projects open through Cloud's relay path", async () => {
  const requests: string[] = [];
  await withCloud(
    (request, response) => {
      const url = new URL(request.url!, "http://cloud");
      requests.push(`${request.method} ${url.pathname}${url.search}`);
      assert.equal(request.headers.authorization, "Bearer account-token");
      const runtime = `/api/v1/remote-hosts/${host}/runtime`;
      if (url.pathname === runtime + "/v1/host/projects")
        return json(response, 200, {
          projects: [{ id: "p1", name: "App", root: "/Users/me/app", git: true, createdAt: "2026-10-01T00:00:00Z" }],
          workspaces: [{ id: "w1", projectId: "p1", name: "app", path: "/Users/me/app", kind: "local", managed: false, createdAt: "2026-10-01T00:00:00Z", lastOpenedAt: "2026-10-22T00:00:00Z", exists: true }],
        });
      if (url.pathname === runtime + "/v1/host/directories")
        return json(response, 200, { path: url.searchParams.get("path"), parent: "/", directories: [] });
      if (url.pathname === runtime + "/v1/host/workspaces/w1/v1/info")
        return json(response, 200, { name: "haarness", schemaVersion: "1", workspace: "/Users/me/app", capabilities: [] });
      if (url.pathname === `/api/v1/remote-hosts/offline/runtime/v1/host/projects`)
        return json(response, 409, { error: { code: "conflict", message: "Remote host is offline or its connection changed. Reconnect and check the current state before retrying.", retryable: false } });
      json(response, 404, { error: { code: "not_found", message: "remote host" } });
    },
    async (url) => {
      const relay = createRelay(async () => ({ token: "account-token", apiUrl: url + "/" }));
      assert.equal((await hostCatalog(relay, host, say)).workspaces[0].id, "w1");
      assert.equal((await hostBrowse(relay, host, say, { path: "/Users/me", hidden: true })).path, "/Users/me");
      const opened = await connectHost(relay, { host, workspace: "w1" }, say);
      opened.client.close();
      assert.deepEqual(
        { ...opened.view, info: undefined },
        {
          id: `host:${host}/workspaces/w1`,
          label: "app",
          workspace: "/Users/me/app",
          remote: true,
          remoteHost: host,
          remoteWorkspace: "w1",
          cloudApiUrl: url,
          trust: false,
          info: undefined,
          project: { id: `host-project:${host}:p1`, name: "App", root: "/Users/me/app", git: true },
          checkout: { id: "w1", name: "app", kind: "local", branch: undefined, managed: false, exists: true },
        },
      );
      // A saved project of the computer reconnects without reading its catalog again.
      const before = requests.length;
      (await connectHost(relay, { host, workspace: "w1", project: opened.view.project, checkout: opened.view.checkout }, say)).client.close();
      assert.deepEqual(requests.slice(before), [`GET /api/v1/remote-hosts/${host}/runtime/v1/host/workspaces/w1/v1/info`]);
      await assert.rejects(hostCatalog(relay, "offline", say), /This computer is offline/);
      await assert.rejects(hostCatalog(relay, "gone", say), /not shared with your Haarness account anymore/);
      await assert.rejects(
        connectHost(relay, { host, workspace: "w1", apiUrl: "https://other.example" }, say),
        /Sign in to the Haarness Cloud this computer was opened from/,
      );
    },
  );
});

test("Cloud's refusals read as plain next steps; anything else keeps its own words", () => {
  assert.match(hostError(new ApiError(401, "unauthorized", "no", false, null), say).message, /Sign in to Haarness/);
  assert.match(hostError(new Error("Error: Sign in to Haarness to continue. Run `haarness login`."), say).message, /open your other computers/);
  assert.equal(hostError(new Error("Project folder is unavailable"), say).message, "Project folder is unavailable");
});

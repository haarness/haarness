import test from "node:test";
import assert from "node:assert/strict";
import {
  createServer,
  type IncomingMessage,
  type ServerResponse,
} from "node:http";
import { once } from "node:events";
import {
  browseRemote,
  remoteCatalog,
  openRemote,
} from "../src/main/remote-projects";

async function withServer(
  respond: (request: IncomingMessage, response: ServerResponse) => void,
  run: (url: string) => Promise<void>,
) {
  const server = createServer(respond).listen(0, "127.0.0.1");
  await once(server, "listening");
  const address = server.address() as { port: number };
  try {
    await run(`http://127.0.0.1:${address.port}`);
  } finally {
    server.closeAllConnections();
    await new Promise<void>((resolve) => server.close(() => resolve()));
  }
}

test("remote folder requests preserve absolute paths and authenticate both browsing and opening", async () => {
  const path = "/work/project 日本 #1&2";
  await withServer(
    (request, response) => {
      assert.equal(request.headers.authorization, "Bearer host-credential");
      const url = new URL(request.url!, "http://localhost");
      response.setHeader("Content-Type", "application/json");
      if (request.method === "GET") {
        assert.equal(url.pathname, "/v1/host/directories");
        assert.equal(url.searchParams.get("path"), path);
        response.end(
          JSON.stringify({ path, parent: "/work", directories: [] }),
        );
      } else {
        assert.equal(url.pathname, "/v1/host/open");
        let body = "";
        request.on("data", (chunk) => {
          body += chunk;
        });
        request.on("end", () => {
          assert.deepEqual(JSON.parse(body), { path });
          response.end(JSON.stringify({ id: "opaque-workspace", path }));
        });
      }
    },
    async (url) => {
      assert.equal(
        (await browseRemote(url, "host-credential", path)).path,
        path,
      );
      assert.equal(
        (await openRemote(url, "host-credential", path)).id,
        "opaque-workspace",
      );
    },
  );
});

test("older servers explain the upgrade and upstream redirects are never followed with host credentials", async () => {
  let redirected = false;
  await withServer(
    (request, response) => {
      if (request.url === "/v1/host/projects") {
        response.writeHead(404);
        response.end();
      } else if (request.url === "/target") {
        redirected = true;
        response.end("{}");
      } else {
        response.writeHead(302, { Location: "/target" });
        response.end();
      }
    },
    async (url) => {
      await assert.rejects(
        remoteCatalog(url, "private"),
        /Update Haarness.*current project remains available/,
      );
      await assert.rejects(browseRemote(url, "private"));
      assert.equal(redirected, false);
    },
  );
  await assert.rejects(
    remoteCatalog("http://remote.example", "private"),
    /HTTPS/,
  );
});

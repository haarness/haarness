import { test } from "node:test";
import assert from "node:assert/strict";
import { mcpServerNote } from "../src/renderer/mcp-server-note";

test("a failed server's reason reads as Haarness wrote it, a server's own words as they are", () => {
  assert.deepEqual(
    mcpServerNote({ status: "failed", detail: "Its URL is not an http or https address with a host." }),
    { text: "Its URL is not an http or https address with a host.", quiet: false },
  );
  assert.deepEqual(
    mcpServerNote({ status: "failed", detail: "Its URL names an environment variable that is not set: DOCS_HOST." }),
    { text: "Its URL names an environment variable that is not set: DOCS_HOST.", quiet: false },
  );
  assert.deepEqual(
    mcpServerNote({ status: "failed", detail: "Its URL names environment variables that are not set: A, B." }),
    { text: "Its URL names environment variables that are not set: A, B.", quiet: false },
  );
  assert.deepEqual(mcpServerNote({ status: "failed", detail: "connection failed: cannot reach http://x/" }), {
    text: "connection failed: cannot reach http://x/",
    quiet: false,
  });
});

test("a working server says only what it left out, quietly", () => {
  assert.equal(mcpServerNote({ status: "connected", detail: "3 tools discovered" }), undefined);
  assert.deepEqual(
    mcpServerNote({
      status: "connected",
      detail: '3 tools discovered; left out as not valid MCP tools: "broken", "odd" +1',
    }),
    { text: 'Left out as not valid MCP tools: "broken", "odd" +1', quiet: true },
  );
  assert.equal(mcpServerNote({ status: "connecting", detail: null }), undefined);
});

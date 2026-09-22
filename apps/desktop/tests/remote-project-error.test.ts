import test from "node:test";
import assert from "node:assert/strict";
import { remoteProjectError } from "../src/renderer/remote-project-error";

test("remote project failures give relevant next steps without exposing server diagnostics", () => {
  assert.match(
    remoteProjectError(
      new Error("Haarness server did not become ready within 8 seconds"),
    ),
    /server took too long/i,
  );
  assert.match(
    remoteProjectError(
      new Error("workspace is not a directory: /private/internal"),
    ),
    /Choose another folder/,
  );
  assert.match(
    remoteProjectError(new Error("Permission denied (os error 13)")),
    /permissions on the server/,
  );
  assert.match(
    remoteProjectError(new Error("401 Unauthorized")),
    /username and password/,
  );
  assert.match(
    remoteProjectError(
      new Error("needs an upgrade but still owns 1 active turn(s)"),
    ),
    /server is busy/,
  );
  assert.match(
    remoteProjectError(
      new Error("This server does not support remote project browsing yet"),
    ),
    /Update Haarness/,
  );
  const unknown = remoteProjectError(
    new Error("Internal failure at /private/internal: SECRET"),
  );
  assert.match(unknown, /server is running/);
  assert.doesNotMatch(unknown, /private|SECRET|Check the path/);
});

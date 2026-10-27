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
      new Error(
        "Haarness server 4242 needs an upgrade (server executable differs from this client build) but still has 1 active turn; finish or close them",
      ),
    ),
    /server is busy/,
  );
  // A terminal open in it keeps a server as busy as a turn does.
  assert.match(
    remoteProjectError(
      new Error(
        "Haarness server 4242 needs an upgrade (server executable differs from this client build) but still has 1 open terminal (npm run dev); finish or close them",
      ),
    ),
    /server is busy/,
  );
  // A host gateway's plain answer for a project another build is running.
  assert.match(
    remoteProjectError(
      new Error(
        "This project is busy on a different version of Haarness. Open it on the computer, or wait until its work finishes.",
      ),
    ),
    /server is busy/,
  );
  // ...and for one of this build working with the other project setting.
  assert.match(
    remoteProjectError(
      new Error(
        "This project is busy with a different project configuration setting. Open it on the computer, or wait until its work finishes.",
      ),
    ),
    /server is busy/,
  );
  assert.match(
    remoteProjectError(
      new Error(
        "cannot safely replace Haarness server 4242 (server executable differs from this client build) because it did not say in time whether it still works",
      ),
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
  // A computer or server that is not answering says so, as everywhere.
  assert.equal(
    remoteProjectError(new Error("Your computer isn't reachable. Open Haarness on it, then try again.")),
    "Your computer isn't reachable. Open Haarness on it, then try again.",
  );
  assert.equal(remoteProjectError(new Error("HTTP 503")), "Haarness isn't answering. Try again in a moment.");
});

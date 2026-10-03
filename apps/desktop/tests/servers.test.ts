import test from "node:test";
import assert from "node:assert/strict";
import { cloudServers, migrateServers, serverKey, serverProjects } from "../src/servers";
import { groupProjects } from "../src/projects";
import type { Recent } from "../src/shared";

const local: Recent = {
  id: "local:/code/app",
  label: "app",
  workspace: "/code/app",
  remote: false,
  trust: true,
};
const remote: Recent = {
  ...local,
  id: "remote:https://one.example",
  remote: true,
  url: "https://one.example/",
  label: "Build server",
};

test("legacy remote projects migrate once without replacing server names or losing servers with no projects", () => {
  const saved = migrateServers([local, remote]);
  assert.deepEqual(saved, [
    { id: remote.id, label: "Build server", url: "https://one.example" },
  ]);
  const renamed = [{ ...saved[0], label: "Production" }];
  assert.deepEqual(migrateServers([remote], renamed), renamed);
  assert.deepEqual(migrateServers([], renamed), renamed);
});

test("project scopes separate local daemons and distinct remote origins with the same directory", () => {
  const other = {
    ...remote,
    id: "remote:https://two.example",
    url: "https://two.example",
  };
  const localOther = {
    ...local,
    id: "local:/code/second",
    workspace: "/code/second",
  };
  const recent = [local, remote, other, localOther];
  assert.deepEqual(serverProjects(recent, "local"), [local, localOther]);
  assert.deepEqual(serverProjects(recent, remote.id), [remote]);
  assert.deepEqual(serverProjects(recent, other.id), [other]);
  assert.equal(
    groupProjects(serverProjects(recent, other.id))[0].id,
    other.id,
  );
  assert.equal(serverKey({ ...remote, url: undefined }), remote.id);
  assert.deepEqual(serverProjects(recent, "missing"), []);
});

test("multiple remote folders share a saved server without sharing project or conversation identity", () => {
  const second: Recent = {
    ...remote,
    id: remote.id + "/workspaces/second",
    remoteWorkspace: "second",
    workspace: "/code/second",
    label: "second",
  };
  const otherHost: Recent = {
    ...second,
    id: "remote:https://two.example/workspaces/second",
    url: "https://two.example",
  };
  const catalog = [local, remote, second, otherHost];
  assert.equal(serverKey(second), remote.id);
  assert.deepEqual(serverProjects(catalog, remote.id), [remote, second]);
  assert.equal(groupProjects(serverProjects(catalog, remote.id)).length, 2);
  assert.equal(migrateServers(catalog).length, 2);
  assert.equal(
    migrateServers(catalog).filter((server) => server.id === remote.id)
      .length,
    1,
  );
});

test("Cloud sessions of one account form one Haarness Cloud server, never a saved gateway", () => {
  const session = (id: string, account = "acct-1"): Recent => ({
    ...local,
    id: `cloud-session:https://api.example/:${account}:${id}`,
    label: "Session " + id,
    remote: true,
    url: `https://gateway.example/ws/${id}`,
    cloudSession: id,
    cloudAccount: account,
    cloudApiUrl: "https://api.example/",
  });
  const recent = [local, remote, session("a"), session("b"), session("c", "acct-2")];
  const key = serverKey(session("a"));
  assert.equal(key, "cloud:https://api.example/:acct-1");
  assert.equal(serverKey(session("b")), key);
  assert.deepEqual(
    serverProjects(recent, key).map((item) => item.cloudSession),
    ["a", "b"],
  );
  assert.deepEqual(cloudServers(recent), [
    { id: key, label: "Haarness Cloud", cloud: true },
    { id: "cloud:https://api.example/:acct-2", label: "Haarness Cloud", cloud: true },
  ]);
  // A gateway saved by an earlier version disappears; real servers stay.
  const stale = { id: "remote:https://gateway.example", label: "Session a", url: "https://gateway.example" };
  assert.deepEqual(migrateServers(recent, [stale]), [
    { id: remote.id, label: "Build server", url: "https://one.example" },
  ]);
});

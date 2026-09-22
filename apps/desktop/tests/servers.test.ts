import test from "node:test";
import assert from "node:assert/strict";
import { migrateServers, serverKey, serverProjects } from "../src/servers";
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

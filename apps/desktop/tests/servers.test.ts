import test from "node:test";
import assert from "node:assert/strict";
import {
  cloudServers,
  localServer,
  migrateServers,
  serverAfterRemoval,
  serverKey,
  serverProjects,
  sortComputers,
} from "../src/servers";
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

test("a window leaving a removed project stays on its server while it is listed, else goes to this computer", () => {
  const remote = { id: "remote:https://host", label: "Host", url: "https://host" };
  assert.equal(serverAfterRemoval([localServer, remote], remote.id), remote.id);
  // The last project of a Cloud account takes the account's entry with it.
  assert.equal(
    serverAfterRemoval([localServer, remote, ...cloudServers([])], "cloud::account"),
    "local",
  );
});

test("each of your other computers is a server of its own, apart from Cloud sessions and saved servers", () => {
  const project = (host: string, workspace: string): Recent => ({
    ...local,
    id: `host:${host}/workspaces/${workspace}`,
    label: workspace,
    remote: true,
    remoteHost: host,
    remoteWorkspace: workspace,
    cloudApiUrl: "https://api.example",
  });
  const studio = [project("h1", "app"), project("h1", "docs")];
  const recent = [local, remote, ...studio, project("h2", "app")];
  assert.equal(serverKey(studio[0]), "host:h1");
  assert.deepEqual(serverProjects(recent, "host:h1"), studio);
  assert.deepEqual(cloudServers(recent), []);
  // A saved computer has no address: migrating keeps it as it is.
  const saved = [{ id: "host:h1", label: "Studio", host: "h1" }];
  assert.deepEqual(migrateServers(recent, saved), [
    ...saved,
    { id: remote.id, label: "Build server", url: "https://one.example" },
  ]);
});

test("your other computers list the connected ones first, by name, then the away ones, last seen first", () => {
  const computers = [
    { id: "1", name: "zeta", connected: true },
    { id: "2", name: "Old box", connected: false, lastSeenAt: "2026-09-01T00:00:00Z" },
    { id: "3", name: "Alpha", connected: true },
    { id: "4", name: "Laptop", connected: false, lastSeenAt: "2026-10-21T00:00:00Z" },
    { id: "5", name: "Never seen", connected: false, lastSeenAt: null },
  ];
  assert.deepEqual(
    sortComputers(computers, "en").map((item) => item.name),
    ["Alpha", "zeta", "Laptop", "Old box", "Never seen"],
  );
  assert.equal(computers[0].name, "zeta", "the list it was given is left alone");
});

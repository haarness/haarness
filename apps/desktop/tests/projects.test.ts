import { test } from "node:test";
import assert from "node:assert/strict";
import type { WorkspaceListResponse, Thread } from "@haarness/sdk";
import type { Recent, Bridge } from "../src/shared";
import {
  cleanProjectMeta,
  groupProjects,
  mergeProjectCatalog,
  migrateCloudSessions,
  migrateProjectPresentation,
  migrateProjectlessLocations,
  primaryLocation,
  projectLocations,
  projectSections,
  readProjectMeta,
  rememberConnected,
  saveConnected,
  sessionKey,
  sidebarGroups,
} from "../src/projects";
import {
  readProjectCache,
  writeProjectCache,
} from "../src/renderer/project-cache";

const catalog: WorkspaceListResponse = {
  projects: [
    { id: "p", name: "app", root: "/code/app", git: true, createdAt: "now" },
  ],
  workspaces: [
    {
      id: "main",
      projectId: "p",
      name: "app",
      path: "/code/app",
      kind: "local",
      managed: false,
      exists: true,
      createdAt: "now",
      lastOpenedAt: "now",
    },
    {
      id: "tree",
      projectId: "p",
      name: "feature",
      path: "/checkouts/feature",
      kind: "git_worktree",
      managed: true,
      exists: true,
      createdAt: "now",
      lastOpenedAt: "now",
    },
  ],
};
const recent = (path: string): Recent => ({
  id: "local:" + path,
  label: path.split("/").at(-1)!,
  workspace: path,
  remote: false,
  trust: true,
});

test("a connected project replaces its saved entry or joins the list, but one removed while it connected stays removed", () => {
  const one = { id: "local:/one", label: "one" };
  const two = { id: "remote:https://host", label: "two" };
  assert.deepEqual(saveConnected([one], { ...one, label: "renamed" }), [
    { ...one, label: "renamed" },
  ]);
  assert.deepEqual(saveConnected([one], two), [one, two]);
  // Asked for by its saved ID, and still saved.
  assert.deepEqual(saveConnected([one, two], two, two.id), [one, two]);
  // Asked for by its saved ID, and removed from the sidebar meanwhile.
  assert.equal(saveConnected([one], two, two.id), undefined);
});

test("a project removed while its folder is looked up after it connected, or while it is saved, is not watched again", async () => {
  const one = { id: "local:/one" };
  let recent = [one];
  const run = async (remove?: "save" | "after") => {
    recent = [];
    let watched = false;
    const kept = await rememberConnected({
      recent: () => recent,
      saved: one,
      save: async (next) => {
        recent = next;
        if (remove === "save") recent = [];
      },
      after: async () => {
        if (remove === "after") recent = [];
      },
      watch: () => {
        watched = true;
      },
    });
    return { kept, watched };
  };
  assert.deepEqual(await run(), { kept: true, watched: true });
  assert.deepEqual(recent, [one]);
  assert.deepEqual(await run("after"), { kept: false, watched: false });
  assert.deepEqual(await run("save"), { kept: false, watched: false });
  // Removed before it connected: nothing is saved either.
  let saved = false;
  assert.equal(
    await rememberConnected({
      recent: () => [],
      saved: one,
      requested: one.id,
      save: async () => {
        saved = true;
      },
      watch: () => assert.fail("watched"),
    }),
    false,
  );
  assert.equal(saved, false);
});

test("registered identity groups a worktree outside the project and discovers the primary folder without conflating remote hosts", () => {
  const remote = {
    ...recent("/code/app"),
    id: "remote:host",
    remote: true,
    url: "https://host",
  };
  const merged = mergeProjectCatalog(
    [recent("/checkouts/feature"), remote],
    catalog,
  );
  const groups = groupProjects(merged);
  assert.equal(groups.length, 2);
  assert.deepEqual(
    groups[0].locations.map((item) => item.workspace),
    ["/checkouts/feature", "/code/app"],
  );
  assert.equal(primaryLocation(groups[0]).workspace, "/code/app");
  assert.equal(groups[1].id, remote.id);
  assert.deepEqual(mergeProjectCatalog(merged, catalog), merged);
});

test("presentation migration preserves group order, expands either previously expanded checkout, and qualifies duplicate imported session IDs", () => {
  const tree = recent("/checkouts/feature"),
    main = recent("/code/app"),
    other = recent("/notes");
  const merged = mergeProjectCatalog([main, other, tree], catalog);
  const migrated = migrateProjectPresentation(
    merged,
    {
      "list:projects": [other.id, tree.id, main.id],
      ["list:sessions:" + tree.id]: ["same", "tree-only"],
      ["list:sessions:" + main.id]: ["same", "root-only"],
    },
    { [main.id]: false, [tree.id]: true },
  );
  assert.deepEqual(migrated.ordering["list:projects"], [
    other.id,
    "local-project:p",
  ]);
  assert.equal(migrated.expanded["local-project:p"], true);
  assert.deepEqual(migrated.ordering["list:sessions:local-project:p"], [
    sessionKey(main.id, "same"),
    sessionKey(main.id, "root-only"),
    sessionKey(tree.id, "same"),
    sessionKey(tree.id, "tree-only"),
  ]);
  assert.deepEqual(
    migrateProjectPresentation(merged, migrated.ordering, migrated.expanded),
    migrated,
  );
});

test("missing worktrees retain their conversation identity without becoming the default working folder", () => {
  const values = mergeProjectCatalog([recent("/checkouts/feature")], {
    ...catalog,
    workspaces: catalog.workspaces.map((item) => ({
      ...item,
      exists: item.id !== "tree",
    })),
  });
  assert.equal(values[0].checkout?.exists, false);
  assert.equal(
    primaryLocation(groupProjects(values)[0]).workspace,
    "/code/app",
  );
});

test("paged conversation cache preserves more than 1000 titles, migrates legacy state and does not publish incomplete writes", async () => {
  const data = new Map<string, unknown>();
  let fail = false;
  const storage: Bridge["storage"] = async (key, value) => {
    if (value !== undefined) {
      assert.ok(Buffer.byteLength(JSON.stringify(value)) < 2 * 1024 * 1024);
      if (fail && key.endsWith(":1")) throw new Error("disk failure");
      if (value === null) data.delete(key);
      else data.set(key, structuredClone(value));
    }
    return data.get(key) ?? null;
  };
  const values = Array.from(
    { length: 1300 },
    (_, i) =>
      ({
        id: String(i),
        title: "Conversation " + i + "x".repeat(500),
      }) as Thread,
  );
  data.set("project-index:v1:test", { version: 1, threads: [values[0]] });
  assert.deepEqual(await readProjectCache(storage, "test"), [values[0]]);
  await writeProjectCache(storage, "test", values);
  assert.deepEqual(await readProjectCache(storage, "test"), values);
  fail = true;
  await assert.rejects(
    writeProjectCache(storage, "test", values.toReversed()),
    /disk failure/,
  );
  assert.deepEqual(await readProjectCache(storage, "test"), values);
  fail = false;
  await Promise.all([
    writeProjectCache(storage, "test", values.toReversed()),
    writeProjectCache(storage, "test", [values[1001]]),
  ]);
  assert.deepEqual(await readProjectCache(storage, "test"), [values[1001]]);
});

test("discovering a Git parent preserves the existing project's place and conversation order", () => {
  const old: Recent = {
    ...recent("/code/app/src"),
    project: {
      id: "old-project",
      root: "/code/app/src",
      name: "src",
      git: false,
    },
  };
  const current: Recent = {
    ...old,
    project: { id: "git-project", root: "/code/app", name: "app", git: true },
  };
  const keys = [sessionKey(old.id, "second"), sessionKey(old.id, "first")];
  const result = migrateProjectPresentation(
    [current],
    {
      "list:projects": ["other", "old-project"],
      "list:sessions:old-project": keys,
    },
    { "old-project": true },
    [old],
  );
  assert.deepEqual(result.ordering["list:projects"], ["other", "git-project"]);
  assert.deepEqual(result.ordering["list:sessions:git-project"], keys);
  assert.equal(result.expanded["git-project"], true);
});

test("legacy chats become ordinary projects without changing identity or trust", () => {
  const chats = {
    ...recent("/code/app"),
    projectless: true,
    label: "Chats",
    trust: false,
  };
  const migrated = migrateProjectlessLocations([chats]);
  assert.equal(chats.projectless, true);
  assert.equal("projectless" in migrated[0], false);
  assert.equal(migrated[0].id, chats.id);
  assert.equal(migrated[0].workspace, chats.workspace);
  assert.equal(migrated[0].trust, false);
  const merged = mergeProjectCatalog(migrated, catalog);
  const groups = groupProjects(merged);
  assert.equal(groups.length, 1);
  assert.equal(groups[0].id, "local-project:p");
  assert.equal(groups[0].label, "app");
  assert.equal(groups[0].locations[0].id, chats.id);
  assert.equal(migrateProjectlessLocations(migrated), migrated);
});

test("projects saved as Cloud sessions keep their ID and open by their workspace", () => {
  const saved = {
    ...recent("/workspace"),
    id: "cloud-session:https://api.example/:account:session-id",
    remote: true,
    cloudSession: "session-id",
    cloudWorkspace: "workspace-id",
  };
  const [migrated] = migrateCloudSessions([saved]);
  assert.equal("cloudSession" in migrated, false);
  assert.equal(migrated.id, saved.id);
  assert.equal(migrated.cloudWorkspace, "workspace-id");
  // One saved before it knew its workspace opens by the old ID, which Cloud still accepts.
  const [older] = migrateCloudSessions([{ ...saved, cloudWorkspace: undefined }]);
  assert.equal(older.cloudWorkspace, "session-id");
  const current = [recent("/code/app")];
  assert.equal(migrateCloudSessions(current), current);
});

test("the chat home is never a project, and the catalog leaves it as it is", () => {
  const home: Recent = {
    ...recent("/data/Chat folders"),
    label: "Chat folders",
    trust: false,
    chatHome: true,
  };
  const app = recent("/code/app");
  // Even a catalog that names its folder does not make it a project.
  const listed: WorkspaceListResponse = {
    ...catalog,
    workspaces: [
      ...catalog.workspaces,
      { ...catalog.workspaces[0], id: "home", path: home.workspace, name: "Chat folders" },
    ],
  };
  const merged = mergeProjectCatalog([app, home], listed);
  assert.deepEqual(merged.find((item) => item.id === home.id), home);
  assert.deepEqual(
    projectLocations(merged).map((item) => item.id),
    merged.filter((item) => item.id !== home.id).map((item) => item.id),
  );
  assert.deepEqual(
    groupProjects(projectLocations(merged)).map((item) => item.label),
    ["app"],
  );
});

test("project presentation keeps only meaningful fields and follows a location into its registered project", () => {
  assert.deepEqual(readProjectMeta(null), {});
  assert.deepEqual(readProjectMeta({ version: 2, projects: { a: { pinned: true } } }), {});
  assert.deepEqual(
    readProjectMeta({
      version: 1,
      projects: {
        a: { name: "  Work  ", pinned: true, section: "" },
        b: { pinned: false, name: "" },
        c: 5,
        d: { section: "Clients", extra: 1 },
      },
    }),
    { a: { name: "Work", pinned: true }, d: { section: "Clients" } },
  );
  assert.equal(cleanProjectMeta({ name: " ", pinned: "yes" }), undefined);
  const main = recent("/code/app");
  const merged = mergeProjectCatalog([main], catalog);
  const migrated = migrateProjectPresentation(merged, {}, {}, [main], {
    [main.id]: { pinned: true, section: "Clients" },
  });
  assert.deepEqual(migrated.meta, {
    "local-project:p": { pinned: true, section: "Clients" },
  });
});

test("the sidebar lists each section in name order, then unfiled projects, with pins at the top of their group", () => {
  const meta = {
    a: { pinned: true, section: "Work" },
    c: { section: "Work" },
    d: { section: "Clients" },
    e: { name: "Renamed" },
    f: { pinned: true },
  };
  assert.deepEqual(projectSections(meta), ["Clients", "Work"]);
  assert.deepEqual(sidebarGroups(["e", "d", "c", "b", "f", "a"], meta), {
    sections: [
      { name: "Clients", ids: ["d"] },
      { name: "Work", ids: ["a", "c"] },
    ],
    rest: ["f", "e", "b"],
  });
  assert.deepEqual(sidebarGroups(["b", "a"], {}), { sections: [], rest: ["b", "a"] });
  // Saved presentation remains available when projects return, but sections
  // from a removed project or another server must not leave empty headings.
  assert.deepEqual(sidebarGroups(["b", "c"], meta), {
    sections: [{ name: "Work", ids: ["c"] }], rest: ["b"],
  });
  assert.deepEqual(sidebarGroups(["b"], meta), { sections: [], rest: ["b"] });
  assert.deepEqual(sidebarGroups([], meta), { sections: [], rest: [] });
});

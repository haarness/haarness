import type { WorkspaceListResponse } from "@haarness/sdk";
import type { ProjectLocation, Recent } from "./shared";

export interface DesktopProject {
  id: string;
  label: string;
  root: string;
  git: boolean;
  remote: boolean;
  locations: Recent[];
}

/** Older general chats already have a durable folder and connection identity.
 * Expose that folder as an ordinary project without moving data or granting trust.
 */
export function migrateProjectlessLocations(
  recent: (Recent & { projectless?: boolean })[],
): Recent[] {
  if (!recent.some((item) => "projectless" in item)) return recent;
  return recent.map(({ projectless: _legacy, ...location }) => location);
}

export const projectKey = (location: ProjectLocation & { id: string }) =>
  location.project?.id ?? location.id;
export const sessionKey = (connection: string, thread: string) =>
  JSON.stringify([connection, thread]);
export const isWorktree = (location?: ProjectLocation) =>
  location?.checkout?.kind === "git_worktree";
export const locationName = (location?: Recent) =>
  isWorktree(location)
    ? location!.checkout!.name
    : location?.project && location.workspace !== location.project.root
      ? "Folder: " + location.label
      : "Project folder";

export function migrateProjectPresentation(
  recent: Recent[],
  ordering: Record<string, string[]>,
  expanded: Record<string, boolean>,
  previous: Recent[] = [],
  meta: ProjectMetaMap = {},
) {
  const projects = groupProjects(recent);
  const keys = new Map(recent.map((item) => [item.id, projectKey(item)]));
  for (const item of previous) {
    const current = recent.find((value) => value.id === item.id);
    if (current) keys.set(projectKey(item), projectKey(current));
  }
  // A pin, name or section follows its project when the catalog resolves a
  // bare location into a registered project.
  const presentation: ProjectMetaMap = {};
  for (const [id, value] of Object.entries(meta))
    presentation[keys.get(id) ?? id] = value;
  const lists = { ...ordering };
  lists["list:projects"] = [
    ...new Set(
      (ordering["list:projects"] ?? recent.map((item) => item.id)).map(
        (id) => keys.get(id) ?? id,
      ),
    ),
  ];
  const expansion = { ...expanded };
  for (const project of projects) {
    if (expansion[project.id] === undefined) {
      const known = project.locations
        .map(
          (item) =>
            expanded[item.id] ??
            expanded[
              projectKey(previous.find((old) => old.id === item.id) ?? item)
            ],
        )
        .filter((value) => value !== undefined);
      if (known.length) expansion[project.id] = known.some(Boolean);
    }
    const scope = "list:sessions:" + project.id;
    const current = ordering[scope] ?? [];
    const inherited = project.locations.flatMap((location) =>
      (
        ordering[
          "list:sessions:" +
            (previous.find((item) => item.id === location.id)?.project?.id ??
              location.id)
        ] ??
        ordering["list:sessions:" + location.id] ??
        []
      ).map((id) => (id.startsWith("[") ? id : sessionKey(location.id, id))),
    );
    if (current.length || inherited.length)
      lists[scope] = [
        ...new Set([
          ...current.filter((id) => id.startsWith("[")),
          ...inherited,
        ]),
      ];
  }
  return { ordering: lists, expanded: expansion, meta: presentation };
}

export function groupProjects(locations: Recent[]): DesktopProject[] {
  const groups = new Map<string, DesktopProject>();
  for (const location of locations) {
    const id = projectKey(location);
    let group = groups.get(id);
    if (!group) {
      group = {
        id,
        label: location.project?.name ?? location.label,
        root: location.project?.root ?? location.workspace,
        git: location.project?.git ?? false,
        remote: location.remote,
        locations: [],
      };
      groups.set(id, group);
    }
    group.locations.push(location);
  }
  return [...groups.values()];
}

export function primaryLocation(project: DesktopProject) {
  return (
    project.locations.find((item) => item.workspace === project.root) ??
    project.locations.find((item) => !isWorktree(item)) ??
    project.locations[0]
  );
}

// Runtime project IDs are authoritative. Paths identify their concrete
// endpoints, never the relationship between a chat and its project.
export function mergeProjectCatalog(
  recent: Recent[],
  catalog: WorkspaceListResponse,
): Recent[] {
  const byPath = new Map(catalog.workspaces.map((item) => [item.path, item]));
  const projects = new Map(catalog.projects.map((item) => [item.id, item]));
  const opened = new Set(
    recent
      .filter((item) => !item.remote)
      .map((item) => byPath.get(item.workspace)?.projectId)
      .filter(Boolean),
  );
  const annotate = (item: Recent): Recent => {
    if (item.remote) return item;
    const checkout = byPath.get(item.workspace);
    const project = checkout && projects.get(checkout.projectId);
    if (!checkout || !project) return item;
    return {
      ...item,
      project: {
        id: "local-project:" + project.id,
        name: project.name,
        root: project.root,
        git: project.git,
      },
      checkout: {
        id: checkout.id,
        name: checkout.name,
        kind: checkout.kind,
        branch: checkout.branch,
        managed: checkout.managed,
        exists: checkout.exists,
      },
    };
  };
  const values = recent.map(annotate);
  const known = new Set(values.map((item) => item.id));
  for (const checkout of catalog.workspaces) {
    if (!opened.has(checkout.projectId) || !checkout.exists) continue;
    const id = "local:" + checkout.path;
    if (known.has(id)) continue;
    const sibling = values.find(
      (item) => item.project?.id === "local-project:" + checkout.projectId,
    );
    values.push(
      annotate({
        id,
        label: checkout.name,
        workspace: checkout.path,
        remote: false,
        trust: sibling?.trust ?? true,
      }),
    );
    known.add(id);
  }
  return values;
}

/**
 * What a person adds to a project for their own sidebar: a display name, a
 * pin and the section it sits in. This is desktop presentation stored under
 * `project-presentation:v1`, keyed by the stable project ID; the runtime's
 * project registry never sees it.
 */
export interface ProjectMeta {
  name?: string;
  pinned?: boolean;
  section?: string;
}
export type ProjectMetaMap = Record<string, ProjectMeta>;

/** Keeps only well-typed, non-empty fields; undefined when nothing is left. */
export function cleanProjectMeta(value: unknown): ProjectMeta | undefined {
  if (!value || typeof value !== "object") return undefined;
  const raw = value as Record<string, unknown>;
  const meta: ProjectMeta = {};
  if (typeof raw.name === "string" && raw.name.trim())
    meta.name = raw.name.trim();
  if (raw.pinned === true) meta.pinned = true;
  if (typeof raw.section === "string" && raw.section.trim())
    meta.section = raw.section.trim();
  return Object.keys(meta).length ? meta : undefined;
}

export function readProjectMeta(value: unknown): ProjectMetaMap {
  if (!value || typeof value !== "object") return {};
  const raw = value as { version?: unknown; projects?: unknown };
  if (raw.version !== 1 || !raw.projects || typeof raw.projects !== "object")
    return {};
  return Object.fromEntries(
    Object.entries(raw.projects as Record<string, unknown>).flatMap(
      ([id, entry]) => {
        const meta = cleanProjectMeta(entry);
        return meta ? [[id, meta]] : [];
      },
    ),
  );
}

/** Every section in use, in a stable alphabetical order. A section exists
 * exactly as long as a project is filed under it. */
export function projectSections(meta: ProjectMetaMap) {
  return [
    ...new Set(
      Object.values(meta).flatMap((item) => (item.section ? [item.section] : [])),
    ),
  ].sort((a, b) => a.localeCompare(b));
}

/**
 * How the sidebar lays out the ordered project list: each section under its
 * heading, then the projects filed under none. A pin keeps a project at the
 * top of whichever group it is in, so filing and pinning always have a
 * visible effect. Inside every group the person's own ordering still applies,
 * so dragging keeps working across the whole list.
 */
export function sidebarGroups(ids: string[], meta: ProjectMetaMap) {
  const pinnedFirst = (list: string[]) => [
    ...list.filter((id) => meta[id]?.pinned),
    ...list.filter((id) => !meta[id]?.pinned),
  ];
  // Metadata survives forgetting a project and is shared across servers;
  // only groups with projects on the current server belong in this sidebar.
  const sections = projectSections(meta).map((name) => ({
    name,
    ids: pinnedFirst(ids.filter((id) => meta[id]?.section === name)),
  })).filter((section) => section.ids.length);
  const rest = pinnedFirst(ids.filter((id) => !meta[id]?.section));
  return { sections, rest };
}

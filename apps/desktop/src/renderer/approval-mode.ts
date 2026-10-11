import type { PendingPermissionInfo, Thread } from "@haarness/sdk";
import { isExplicitPermission } from "../chrome";

export type ApprovalModes = Record<string, Record<string, boolean>>;
export function readApprovalModes(value: unknown): ApprovalModes {
  if (!value || typeof value !== "object" || Array.isArray(value)) return {};
  return Object.fromEntries(
    Object.entries(value).flatMap(([key, modes]) =>
      modes && typeof modes === "object" && !Array.isArray(modes)
        ? [
            [
              key,
              Object.fromEntries(
                Object.entries(modes).filter(
                  (entry): entry is [string, boolean] =>
                    typeof entry[1] === "boolean",
                ),
              ),
            ],
          ]
        : [],
    ),
  );
}
export function autoApproval(
  modes: ApprovalModes,
  connection: string,
  thread: string,
  threads: Thread[] = [],
  root?: string | null,
): boolean {
  const rules = modes[connection];
  if (!rules) return false;
  const seen = new Set<string>();
  let id: string | undefined = thread;
  while (id && !seen.has(id)) {
    seen.add(id);
    if (rules[id] !== undefined) return rules[id];
    const current = threads.find((item) => item.id === id);
    id =
      (current?.isSubagent ? current.parentId : undefined) ??
      (id !== root ? (root ?? undefined) : undefined);
  }
  return rules["*"] ?? false;
}
/** Whether this client may answer a request for the user. An explicit
 * request (a site in the user's own Chrome) always waits for a person. */
export function canAutoApprove(
  request: Pick<PendingPermissionInfo, "permission" | "operation"> & {
    patterns?: string[];
    allowOnce?: boolean;
    allowAlways?: boolean | null;
    explicit?: boolean | null;
  },
): boolean {
  const category = request.permission || request.operation.split(":")[0];
  return (
    request.allowOnce !== false &&
    request.allowAlways !== false &&
    category !== "instruction_import" &&
    !isExplicitPermission(request)
  );
}

/** One attempt per pending request, including across snapshot/SSE replay.
 * Failed replies stay manual until explicitly retried. Never learns allow rules. */
export function createAutoApprover(input: {
  enabled: (
    connection: string,
    request: PendingPermissionInfo,
    threads: Thread[],
  ) => boolean;
  reply: (connection: string, id: string) => Promise<unknown>;
  failed: (
    connection: string,
    request: PendingPermissionInfo,
    error: unknown,
  ) => void;
}) {
  const attempted = new Map<string, Set<string>>();
  const consume = (
    connection: string,
    requests: PendingPermissionInfo[],
    threads: Thread[],
  ) => {
    let ids = attempted.get(connection);
    if (!ids) attempted.set(connection, (ids = new Set()));
    for (const request of requests) {
      if (
        !canAutoApprove(request) ||
        !input.enabled(connection, request, threads) ||
        ids.has(request.permissionId)
      )
        continue;
      ids.add(request.permissionId);
      // Bound replay memory without allowing a failed, still-pending prompt to retry.
      if (ids.size > 2000) {
        const pending = new Set(requests.map((item) => item.permissionId));
        for (const id of ids) if (!pending.has(id)) ids.delete(id);
      }
      void input
        .reply(connection, request.permissionId)
        .catch((error) => input.failed(connection, request, error));
    }
  };
  return Object.assign(consume, {
    acknowledge(connection: string, id: string) {
      let ids = attempted.get(connection);
      if (!ids) attempted.set(connection, (ids = new Set()));
      ids.add(id);
    },
  });
}

/** Enabling Auto is conditional on a successful reply and the original view/mode. */
export async function approveAndEnable(input: {
  reply: () => Promise<unknown>;
  acknowledge: () => void;
  current: () => boolean;
  enable: () => Promise<unknown>;
}) {
  await input.reply();
  input.acknowledge();
  if (input.current()) await input.enable();
}

/** The plain reading of a stored rule's catch-all, by permission. */
const everyRequest: Record<string, string> = {
  bash: "Every command",
  edit: "Every file edit",
  read: "Every file read",
  webfetch: "Every web page",
  workflow: "Every workflow",
};

/**
 * What an "always" answer lets through, in words. The stored rule keeps its
 * `permission:` prefix and glob (`external_directory:/tmp/notes/*`), which
 * read as internals; the menu says "Anything in …/tmp/notes" instead. A rule
 * with no plainer reading is shown without its prefix.
 */
export function describeAlwaysRule(
  permission: string,
  rule: string,
  words: {
    t: (key: string, values?: Record<string, string>) => string;
    shortPath: (path: string) => string;
  },
): string {
  const { t } = words;
  const pattern =
    permission && permission !== "multiple" && rule.startsWith(permission + ":")
      ? rule.slice(permission.length + 1)
      : rule;
  if (permission === "agent_switch") return t("Only this agent, in this chat");
  if (permission === "external_directory") {
    const folder = pattern.replace(/[/\\]\*$/, "");
    return folder === "*"
      ? t("Any folder outside the project")
      : t("Anything in {folder}", { folder: words.shortPath(folder) });
  }
  if (pattern === "*")
    return everyRequest[permission]
      ? t(everyRequest[permission])
      : t("Every request of this kind");
  if (permission === "bash" && pattern.endsWith(" *"))
    return t("Commands starting with {command}", { command: pattern.slice(0, -2) });
  if (permission === "read") return t("Reading {path}", { path: pattern });
  if (permission === "media_generate" && pattern === "image:*")
    return t("Any image generation");
  if (permission === "media_generate" && pattern === "video:*")
    return t("Any video generation");
  return pattern;
}

import type { PendingPermissionInfo, Thread } from "@haarness/sdk";

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
export function canAutoApprove(
  request: Pick<PendingPermissionInfo, "permission" | "operation"> & {
    allowOnce?: boolean;
    allowAlways?: boolean | null;
  },
): boolean {
  const category = request.permission || request.operation.split(":")[0];
  return (
    request.allowOnce !== false &&
    request.allowAlways !== false &&
    category !== "instruction_import"
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

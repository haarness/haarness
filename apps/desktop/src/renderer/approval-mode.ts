import type {
  ApprovalMode,
  PendingPermissionInfo,
  PermissionReview,
  Thread,
} from "@haarness/sdk";
import { isExplicitPermission } from "../chrome";

/** What a chat's approval mode does: Review asks before risky steps, Auto
 * answers safe requests without asking, Yolo approves every request
 * without asking. */
export type Mode = "review" | "auto" | "yolo";
/** Servers listing this keep a chat in Yolo; others offer Auto and Review. */
export const YOLO_CAPABILITY = "approval-mode.yolo.v1";
/** The order every surface lists the modes in, and cycles through them. */
export const MODES: readonly Mode[] = ["review", "auto", "yolo"];
/** A mode a server or saved data names, read for what it does: one this
 * window does not know (a newer server's) answers nothing, as Review. */
export function readMode(value: unknown): Mode {
  return value === "auto" || value === "yolo" ? value : "review";
}
/** The mode after `mode` when a person cycles: Review, Auto, Yolo, Review.
 * Where the server does not offer Yolo, Auto goes back to Review. */
export function nextMode(mode: Mode, yolo: boolean): Mode {
  if (mode === "review") return "auto";
  return mode === "auto" && yolo ? "yolo" : "review";
}
/** The modes a person may pick: Yolo only where the server offers it, or
 * while the chat is in it already, so its row shows what holds. */
export function offeredModes(yolo: boolean, current?: Mode): Mode[] {
  return MODES.filter((mode) => mode !== "yolo" || yolo || current === "yolo");
}
/** The pill's and the menu's words and drawing for each mode. */
export const modeCopy: Record<Mode, { name: string; detail: string; icon: "approval-review" | "approval-auto" | "approval-yolo" }> = {
  review: { name: "Review", detail: "Asks before risky steps", icon: "approval-review" },
  auto: { name: "Auto", detail: "Answers safe requests without asking", icon: "approval-auto" },
  yolo: { name: "Yolo", detail: "Approves every request without asking", icon: "approval-yolo" },
};

/** The modes chosen on this device, by connection and chat ("*" for new
 * chats). Saved as `true`/`false` for Auto/Review, so an older Desktop still
 * reads them, and as "yolo". */
export type ApprovalModes = Record<string, Record<string, Mode>>;
export function readApprovalModes(value: unknown): ApprovalModes {
  if (!value || typeof value !== "object" || Array.isArray(value)) return {};
  return Object.fromEntries(
    Object.entries(value).flatMap(([key, modes]) =>
      modes && typeof modes === "object" && !Array.isArray(modes)
        ? [
            [
              key,
              Object.fromEntries(
                Object.entries(modes).flatMap(([id, mode]): [string, Mode][] =>
                  typeof mode === "boolean"
                    ? [[id, mode ? "auto" : "review"]]
                    : mode === "auto" || mode === "review" || mode === "yolo"
                      ? [[id, mode]]
                      : [],
                ),
              ),
            ],
          ]
        : [],
    ),
  );
}
/** `modes` as saved: what an older Desktop reads stays a boolean. */
export function savedApprovalModes(modes: ApprovalModes) {
  return Object.fromEntries(
    Object.entries(modes).map(([key, rules]) => [
      key,
      Object.fromEntries(
        Object.entries(rules).map(([id, mode]) => [id, mode === "yolo" ? "yolo" : mode === "auto"]),
      ),
    ]),
  );
}
/** The mode this device chose for `thread` on a server that does not keep
 * modes: its own, its delegating chat's, else the one for new chats. */
export function localApprovalMode(
  modes: ApprovalModes,
  connection: string,
  thread: string,
  threads: Thread[] = [],
  root?: string | null,
): Mode {
  const rules = modes[connection];
  if (!rules) return "review";
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
  return rules["*"] ?? "review";
}
/** Whether this client answers `thread`'s requests itself, on a server that
 * does not keep modes: any mode but Review. */
export function autoApproval(
  modes: ApprovalModes,
  connection: string,
  thread: string,
  threads: Thread[] = [],
  root?: string | null,
): boolean {
  return localApprovalMode(modes, connection, thread, threads, root) !== "review";
}
/** The mode a new chat starts in on a server that keeps modes: the one
 * chosen here for new chats, else the server's own default for them
 * (`defaultApprovalMode`, Review or Yolo where its configuration says so).
 * A server that does not name one starts them in Auto. A Yolo chosen here
 * counts only where the server offers it (`yolo`). */
export function newChatMode(
  modes: ApprovalModes,
  connection: string,
  serverDefault?: ApprovalMode | string | null,
  yolo = true,
): Mode {
  const chosen = modes[connection]?.["*"];
  if (chosen !== undefined && (chosen !== "yolo" || yolo)) return chosen;
  return serverDefault == null ? "auto" : readMode(serverDefault);
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
  /** A reply error that means the request was settled meanwhile. */
  settled?: (error: unknown) => boolean;
}): Promise<boolean> {
  // A request settled meanwhile (Auto's reviewer, another window) still
  // takes the person's choice of Auto: true says it was answered already.
  let answered = false;
  try {
    await input.reply();
  } catch (error) {
    if (!input.settled?.(error)) throw error;
    answered = true;
  }
  input.acknowledge();
  if (input.current()) await input.enable();
  return answered;
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

/** `text` (a path, an error) set apart from the sentence around it, so a
 * right-to-left sentence keeps it in one piece (`/tmp/cache`, not
 * `tmp/cache/`) and its own punctuation in place: FSI … PDI. */
export function isolate(text: unknown): string {
  return "\u2068" + String(text ?? "") + "\u2069";
}

/** Haarness's own words for a note it wrote itself, by its code, in the
 * person's language; undefined for the reviewer's own words or a code this
 * window does not know (its English `rationale` is shown then). */
export function reviewReasonText(
  review: PermissionReview,
  t: (key: string, values?: Record<string, string | number>) => string,
): string | undefined {
  const reason = review.reason;
  switch (reason?.code) {
    case "timeout":
      return t("It took more than {seconds} s.", { seconds: reason.seconds });
    case "too_long":
      return t("The action is too long to review whole.");
    case "no_model":
      return t("No model was available to review this request.");
    case "error":
      return t("It could not decide ({detail}).", { detail: isolate(reason.detail) });
    case "streak":
      return t("It blocked {count} actions in a row, so the rest of this turn is yours to approve.", { count: reason.count });
    case "turn_limit":
      return t("It blocked {count} actions this turn, so the rest of it is yours to approve.", { count: reason.count });
    case "unnamed_deletion":
      return t("This deletes {path}, outside the project, and your messages do not name it.", { path: isolate(reason.path) });
    case "unplaced_deletion":
      return t("This deletes {path}, a path Haarness can't place, and your messages do not name it.", { path: isolate(reason.path) });
    case "unnamed_overwrite":
      return t("This overwrites {path}, outside the project, and your messages do not name it.", { path: isolate(reason.path) });
    case "unplaced_overwrite":
      return t("This overwrites {path}, a path Haarness can't place, and your messages do not name it.", { path: isolate(reason.path) });
    case "unread_script":
      return t("This runs a here-document Haarness could not read with {program}, and your messages do not ask for it.", { program: isolate(reason.program) });
    case "inline_code":
      return t("This runs {program} code that deletes files or reaches outside the project, and your messages do not ask for it.", { program: isolate(reason.program) });
    case "rated":
      return review.risk === "critical"
        ? t("It rated this action critical risk.")
        : review.risk === "low"
          ? t("It rated this action low risk, and your messages do not clearly ask for it.")
          : review.risk === "medium"
            ? t("It rated this action medium risk, and your messages do not clearly ask for it.")
            : t("It rated this action high risk, and your messages do not clearly ask for it.");
    default:
      return undefined;
  }
}

/** Why Auto's reviewer left a request to the person, in one line: "Auto
 * review · <rule>: <reason>", that it was unavailable or paused, or that it
 * is deciding a request already shown. Empty when it did not review it. */
export function reviewNote(
  review: PermissionReview | null | undefined,
  t: (key: string, values?: Record<string, string | number>) => string,
): string {
  const parts = reviewNoteParts(review, t);
  if (!parts) return "";
  return parts.lead + (parts.rule ? " · " + parts.rule : "") + (parts.reason ? ": " + parts.reason : "");
}

/** The note's parts, for a card that sets the rule and the reason apart
 * (`<bdi>`): each keeps its own direction beside the interface's. */
export function reviewNoteParts(
  review: PermissionReview | null | undefined,
  t: (key: string, values?: Record<string, string | number>) => string,
): { lead: string; rule?: string; reason?: string } | undefined {
  if (!review) return undefined;
  if (review.status === "reviewing") return { lead: t("Auto review is deciding…") };
  const lead =
    review.status === "failed"
      ? t("Auto review unavailable")
      : review.status === "paused"
        ? t("Auto review paused")
        : t("Auto review");
  return { lead, rule: reviewRule(review, t), reason: reviewReason(review, t) || undefined };
}

/** The reviewer's rule as a note shows it: Haarness's own ("Destructive
 * action", on a note it wrote itself) in the person's language, the
 * reviewer's as it wrote it. */
export function reviewRule(
  review: PermissionReview,
  t: (key: string, values?: Record<string, string | number>) => string,
): string | undefined {
  const rule = review.rule?.trim();
  if (!rule) return undefined;
  return review.reason ? t(rule) : rule;
}

/** The reason in a note, in the person's language when Haarness wrote it;
 * empty when there is none. */
export function reviewReason(
  review: PermissionReview,
  t: (key: string, values?: Record<string, string | number>) => string,
): string {
  return (reviewReasonText(review, t) ?? review.rationale ?? "").trim();
}

/** A reply the server answered with "not found": the request was settled
 * meanwhile (by Auto's reviewer, a cascade, another window). Only the
 * message crosses from the main process. */
export function alreadySettled(error: unknown): boolean {
  const message = error instanceof Error ? error.message : String(error);
  return /\bnot found: permission\b/i.test(message);
}

/** What the approval pill's mode does, for its tooltip: for the open chat
 * and its agents (`chat`), or for new chats here. In Auto, a server that
 * asks a reviewer (`approval-review.v1`) says so; otherwise Auto approves
 * once. Review is worded after the server's default rules
 * (permissions/mod.rs): it does not ask before every edit or command. */
export function approvalModeHint(
  mode: Mode,
  chat: boolean,
  reviewer: boolean,
  t: (key: string) => string,
): string {
  if (mode === "yolo")
    return chat
      ? t("Yolo: every permission request in this chat and its agents is approved once without asking. Deny rules still hold, and requests only you can answer still ask.")
      : t("Yolo: every permission request in new chats here is approved once without asking. Deny rules still hold, and requests only you can answer still ask.");
  if (mode === "auto") {
    if (reviewer)
      return chat
        ? t("Auto: a reviewer model approves routine permission requests in this chat and its agents once, and asks you about the rest.")
        : t("Auto: a reviewer model approves routine permission requests in new chats here once, and asks you about the rest.");
    return chat
      ? t("Auto: permission requests in this chat and its agents are allowed once without asking.")
      : t("Auto: permission requests in new chats here are allowed once without asking.");
  }
  return chat
    ? t("Review: edits and commands inside the project run without asking. By default this chat and its agents ask before using files outside the project, reading secrets such as .env, switching agents or generating paid media.")
    : t("Review: edits and commands inside the project run without asking. By default new chats here ask before using files outside the project, reading secrets such as .env, switching agents or generating paid media.");
}

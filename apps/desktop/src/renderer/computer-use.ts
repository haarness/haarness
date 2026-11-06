/*
 * Computer use on the Desktop side: the server's `computer-use` resource
 * (Settings → Computer use), the words of the bar the Mac shows while an
 * agent works, and how a `computer` approval reads. Pure, so the rules are
 * pinned in tests rather than found in the panes.
 */
import type { PendingPermissionInfo } from "@haarness/sdk";

export type ComputerRule = "allow" | "ask" | "deny";

export interface ComputerUseStatus {
  supported: boolean;
  platform?: string;
  /** The person running the server is the one logged in at the screen. */
  console: boolean;
  /** On with the current terms agreed. */
  enabled: boolean;
  /** The person's switch, whatever the terms say. */
  switch?: boolean;
  terms: {
    version: string;
    accepted: boolean;
    acceptedAt?: string | null;
    text?: { title: string; intro: string; sections: { heading: string; items: string[] }[] };
  };
  permissions?: { accessibility: boolean; screenRecording: boolean } | null;
  screen?: { locked: boolean; asleep: boolean } | null;
  /** The app macOS charges the permissions to: Haarness, or a terminal. */
  host?: { bundle: string; name?: string | null; haarness?: boolean } | null;
  paused?: boolean;
  active?: { thread: string; here: boolean } | null;
  settings: {
    defaultAct: "ask" | "allow";
    ring: boolean;
    apps: { bundle: string; name?: string | null; rule: ComputerRule }[];
  };
}

/** The capability a server advertises when it has the resource. */
export const COMPUTER_USE_CAPABILITY = "computer-use.v1";

/** The bar's words, as English catalog keys; `{app}` stands where the helper puts the app's name. */
export const barKeys = {
  usingApp: "Haarness is working in {app}",
  usingMac: "Haarness is working on this Mac",
  paused: "Haarness is waiting for you",
  pause: "Pause",
  resume: "Resume",
  stop: "Stop",
  stopHint: "Esc stops it too",
} as const;

/** The bar's words in the interface language, `%@` where the app's name goes. */
export function barWords(translate: (key: string) => string) {
  const words: Record<string, string> = {};
  for (const [field, key] of Object.entries(barKeys)) words[field] = translate(key);
  // A translation that lost its placeholder would name no app: English keeps it.
  words.usingApp = (words.usingApp.includes("{app}") ? words.usingApp : barKeys.usingApp).replace("{app}", "%@");
  return words;
}

/** A bundle identifier as macOS writes it, or a prefix rule such as `com.jetbrains.*`. */
export function validBundle(text: string) {
  return /^[A-Za-z0-9][A-Za-z0-9._-]*(?:\.\*)?$/.test(text.trim()) && text.trim().length <= 255;
}

/** The rules an app may have, as catalog keys. */
export const ruleChoices: { value: ComputerRule; label: string }[] = [
  { value: "allow", label: "Act without asking" },
  { value: "ask", label: "Ask every time" },
  { value: "deny", label: "Never" },
];

/** What happens in apps without a rule. */
export const defaultChoices: { value: "ask" | "allow"; label: string }[] = [
  { value: "ask", label: "Ask first" },
  { value: "allow", label: "Act without asking" },
];

/** The prepared detail a computer use request carries under `__mac`. */
export interface MacDetail {
  kind?: "act" | "launch" | "screen";
  action?: string;
  bundle?: string | null;
  app?: string | null;
  target?: string | null;
  endApp?: string | null;
  preview?: string | null;
  text?: string | null;
  keys?: string | null;
  console?: boolean;
  everyTime?: boolean;
  summary?: string;
}

const record = (value: unknown): Record<string, unknown> | undefined =>
  value !== null && typeof value === "object" && !Array.isArray(value) ? (value as Record<string, unknown>) : undefined;

/** A request to act on this Mac's screen: answered by a person, per app and chat. */
export function isComputerPermission(request: Pick<PendingPermissionInfo, "permission" | "operation">) {
  return (request.permission || request.operation.split(":")[0]) === "computer";
}

export function macDetail(detail: unknown): MacDetail {
  const mac = record(record(detail)?.__mac) ?? {};
  const text = (key: string) => (typeof mac[key] === "string" && (mac[key] as string).trim() ? (mac[key] as string) : null);
  return {
    kind: mac.kind === "launch" || mac.kind === "screen" ? mac.kind : "act",
    action: text("action") ?? undefined,
    bundle: text("bundle"),
    app: text("app"),
    target: text("target"),
    endApp: text("endApp"),
    // Only a JPEG or PNG the server made: never a page's or an app's own address.
    preview: /^data:image\/(?:jpeg|png);base64,[A-Za-z0-9+/=]+$/.test(text("preview") ?? "") ? text("preview") : null,
    text: text("text"),
    keys: text("keys"),
    console: mac.console === true,
    everyTime: mac.everyTime === true,
    summary: text("summary") ?? undefined,
  };
}

/** Typed text on a card: its start, whole lines kept short. */
export function shownText(text: string, limit = 160) {
  const flat = text.replace(/\s*\n\s*/g, " ⏎ ");
  return flat.length > limit ? flat.slice(0, limit).trimEnd() + "…" : flat;
}

/**
 * How a computer use card reads: the line the server wrote for it, the
 * picture of the spot, what the agent would type, and why it asks every
 * time. `translate` is `t`, passed in so the rules stay pure.
 */
export function computerApproval(
  request: Pick<PendingPermissionInfo, "detail" | "allowAlways">,
  translate: (key: string, values?: Record<string, string>) => string,
) {
  const mac = macDetail(request.detail);
  const app = mac.app ?? mac.bundle ?? translate("this app");
  const notes: string[] = [];
  if (mac.kind === "screen")
    notes.push(translate("Everything open on screen, apart from password managers and Haarness, goes to the model."));
  else if (mac.everyTime && mac.console)
    notes.push(translate("Terminal and editor text can hold someone's prompt, so every click on it needs your approval."));
  else if (mac.everyTime) notes.push(translate("You set this app to ask every time."));
  return {
    summary: mac.kind === "screen" ? translate("Look at every app's windows") : (mac.summary ?? app),
    preview: mac.kind === "screen" ? null : mac.preview,
    text: mac.text ? shownText(mac.text) : null,
    keys: mac.text ? null : mac.keys,
    notes,
    /** Label for the chat-wide allow; absent when the request must be approved each time. */
    inChat:
      request.allowAlways !== false && !mac.everyTime
        ? mac.kind === "screen"
          ? translate("Allow in this chat")
          : translate("Allow in {app} for this chat", { app })
        : null,
  };
}

/** Catalog keys this module and its pane reach through variables, for the catalog guard. */
export const computerUseKeys = [
  ...Object.values(barKeys),
  ...ruleChoices.map((choice) => choice.label),
  ...defaultChoices.map((choice) => choice.label),
  "Computer use",
  "this app",
  "Allow in this chat",
  "Allow in {app} for this chat",
  "Look at every app's windows",
  "Everything open on screen, apart from password managers and Haarness, goes to the model.",
  "Terminal and editor text can hold someone's prompt, so every click on it needs your approval.",
  "You set this app to ask every time.",
];

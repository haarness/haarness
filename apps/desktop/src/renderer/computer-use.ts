/*
 * Computer use on the Desktop side: the server's `computer-use` resource
 * (Settings → Computer use), the words of the bar the Mac shows while an
 * agent works, and how a `computer` approval reads. Pure, so the rules are
 * pinned in tests rather than found in the panes.
 */
import type { Item, PendingPermissionInfo, Thread } from "@haarness/sdk";

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
    /** The person agreed to earlier terms, which changed since. Older servers leave it out. */
    changed?: boolean;
    text?: { title: string; intro: string; sections: { heading: string; items: string[] }[] };
  };
  permissions?: { accessibility: boolean; screenRecording: boolean } | null;
  screen?: { locked: boolean; asleep: boolean } | null;
  /** The app macOS charges the permissions to: Haarness, or a terminal. */
  host?: { bundle: string; name?: string | null; haarness?: boolean } | null;
  paused?: boolean;
  /** The chat the person paused on the strip, while the pause lasts. */
  pausedChat?: string | null;
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
  stopHint: "Esc stops it too, while it works in the app in front",
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
  endBundle?: string | null;
  endApp?: string | null;
  preview?: string | null;
  text?: string | null;
  keys?: string | null;
  /** The element's own words, and a scroll's direction: the parts of `summary`. */
  label?: string | null;
  direction?: string | null;
  /** Whether the server sent the parts above: an older one sends only `summary`. */
  parts?: boolean;
  /**
   * A terminal or editor, as servers before terms version 2 mark one: such a server asks every time for a click on its
   * text and never keeps an answer for it.
   */
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
    endBundle: text("endBundle"),
    endApp: text("endApp"),
    // Only a JPEG or PNG the server made: never a page's or an app's own address.
    preview: /^data:image\/(?:jpeg|png);base64,[A-Za-z0-9+/=]+$/.test(text("preview") ?? "") ? text("preview") : null,
    text: text("text"),
    keys: text("keys"),
    label: text("label"),
    direction: text("direction"),
    // A server that sends the line's parts always sends `label`, null for an element without words.
    parts: Object.prototype.hasOwnProperty.call(mac, "label"),
    console: mac.console === true,
    everyTime: mac.everyTime === true,
    summary: text("summary") ?? undefined,
  };
}

/** The card's main line for each action, with the element as `{target}` and its app as `{app}`. */
const actionLines: Record<string, string> = {
  click: "Click {target} in {app}",
  "double-click": "Double-click {target} in {app}",
  "triple-click": "Triple-click {target} in {app}",
  "right-click": "Right-click {target} in {app}",
  "middle-click": "Middle-click {target} in {app}",
  type: "Type into {target} in {app}",
  replace: "Type into {target} in {app}",
  hover: "Point at {target} in {app}",
};
const scrollLines: Record<string, string> = {
  up: "Scroll up over {target} in {app}",
  down: "Scroll down over {target} in {app}",
  left: "Scroll left over {target} in {app}",
  right: "Scroll right over {target} in {app}",
};

/**
 * A part of a card's line as an app gave it (an element's label, an app's name, keys), without the marks and isolates
 * that set the direction of text (U+061C, U+200E, U+200F, U+202A–U+202E, U+2066–U+2069): one could end the part's
 * place in the line early and turn the rest of the line around.
 */
function plainPart<T extends string | null | undefined>(text: T): T {
  return (text?.replace(/[\u061c\u200e\u200f\u202a-\u202e\u2066-\u2069]/g, "") ?? text) as T;
}

/**
 * The card's main line in the person's language, built from the request's parts; null when the server sent an action
 * this build does not know, whose line the server's own words then give.
 */
export function computerLine(mac: MacDetail, translate: (key: string, values?: Record<string, string>) => string) {
  if (!mac.parts) return null;
  const app = plainPart(mac.app ?? mac.bundle) ?? translate("this app");
  const label = plainPart(mac.label);
  const target = label ? translate("“{label}”", { label }) : translate("the marked spot");
  const endApp = plainPart(mac.endApp);
  const keys = plainPart(mac.keys);
  if (mac.kind === "launch") return translate(mac.action === "quit" ? "Quit {app}" : "Open {app}", { app });
  if (mac.action === "press" && keys) return translate("Press {keys} in {app}", { keys, app });
  if (mac.action === "drag")
    return endApp && endApp !== app
      ? translate("Drag {target} from {app} to {endApp}", { target, app, endApp })
      : translate("Drag {target} in {app}", { target, app });
  if (mac.action === "scroll") {
    const line = scrollLines[mac.direction ?? "down"];
    return line ? translate(line, { target, app }) : null;
  }
  const line = mac.action ? actionLines[mac.action] : undefined;
  return line ? translate(line, { target, app }) : null;
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
  request: Pick<PendingPermissionInfo, "detail" | "allowAlways" | "patterns">,
  translate: (key: string, values?: Record<string, string>) => string,
) {
  const mac = macDetail(request.detail);
  const app = mac.app ?? mac.bundle ?? translate("this app");
  const allowed = allowedApp(request);
  const allowedName = allowed?.app ?? allowed?.bundle ?? translate("this app");
  const lasting = lastingApp(request);
  const notes: string[] = [];
  if (mac.kind === "screen")
    notes.push(translate("Everything open on screen, apart from password managers and Haarness, goes to the model."));
  else if (mac.everyTime && mac.console)
    notes.push(translate("Terminal and editor text can hold someone's prompt, so every click on it needs your approval."));
  else if (mac.everyTime) notes.push(translate("You set this app to ask every time."));
  return {
    summary:
      mac.kind === "screen"
        ? translate("Look at every app's windows")
        : (computerLine(mac, translate) ?? plainPart(mac.summary ?? app)),
    preview: mac.kind === "screen" ? null : mac.preview,
    text: mac.text ? shownText(mac.text) : null,
    keys: mac.text ? null : mac.keys,
    notes,
    /** Label for the chat-wide allow; absent when the request must be approved each time. */
    inChat:
      request.allowAlways !== false && !mac.everyTime
        ? lasting || mac.kind === "screen" || !allowed
          ? translate("Allow in this chat")
          : translate("Allow in {app} for this chat", { app: allowedName })
        : null,
    /**
     * Allowing the app in every chat (an Allow rule in Settings → Computer use): offered for an action in an ordinary
     * app, never for opening one, the whole screen, or a request that asks every time.
     */
    always: lasting ? { label: translate("Always allow {app}", { app: allowedName }), bundle: allowed!.bundle! } : null,
    alwaysHint: lasting
      ? translate("Agents won't ask before using {app} again, in any chat. You can change this in Settings → Computer use.", {
          app: allowedName,
        })
      : null,
  };
}

/** The apps a request asks about, by bundle identifier: each of its patterns names one (`act:<chat>:<bundle>`). */
function askedBundles(request: Pick<PendingPermissionInfo, "patterns">) {
  const bundles = (request.patterns ?? []).map((pattern) => /^(?:computer:)?act:[^:]+:([^:]+)$/.exec(pattern)?.[1]);
  return [...new Set(bundles.filter((bundle): bundle is string => !!bundle))];
}

/**
 * The one app an answer to a request allows: the app it acts in or, for a drag into another app, whichever of the two
 * it asks about. None for a drag that asks about both, where naming one app would leave the other unsaid.
 */
function allowedApp(request: Pick<PendingPermissionInfo, "detail" | "patterns">) {
  const mac = macDetail(request.detail);
  if (mac.action !== "drag" || !mac.endBundle || mac.endBundle === mac.bundle) return { bundle: mac.bundle, app: mac.app };
  const asked = askedBundles(request);
  if (asked.length !== 1) return null;
  if (asked[0] === mac.endBundle) return { bundle: mac.endBundle, app: mac.endApp };
  return asked[0] === mac.bundle ? { bundle: mac.bundle, app: mac.app } : null;
}

/** Whether a computer use request may allow its app in every chat. */
export function lastingApp(request: Pick<PendingPermissionInfo, "detail" | "allowAlways" | "patterns">) {
  const mac = macDetail(request.detail);
  return request.allowAlways !== false && mac.kind === "act" && !mac.everyTime && !mac.console && !!allowedApp(request)?.bundle;
}

/** The chat a `computer-use` answer (its view's `data`) says the person paused, if any. */
export function pausedChatIn(answer: unknown): string | undefined {
  const chat = record(record(answer)?.data)?.pausedChat;
  return typeof chat === "string" && chat ? chat : undefined;
}

/**
 * Whether the chat on screen is the one paused on the strip: the paused chat is the conversation's root, so a child
 * agent's chat counts too when it was started from it.
 */
export function pausedHere(paused: string | undefined, selected: string, threads: Pick<Thread, "id" | "parentId">[]) {
  if (!paused || !selected) return false;
  const parents = new Map(threads.map((thread) => [thread.id, thread.parentId]));
  let chat: string | null | undefined = selected;
  // A chat tree is shallow; the limit only guards against a cycle in bad data.
  for (let depth = 0; chat && depth < 32; depth++) {
    if (chat === paused) return true;
    chat = parents.get(chat);
  }
  return false;
}

/**
 * Whether a turn waits on the screen: one of its `mac_*` calls has no result yet and waits for no approval (`asking`,
 * the calls of the requests a person has still to answer). Only then does a pause on the strip hold it; otherwise the
 * turn's own work (a command, an edit, its reply, an approval card) shows as usual.
 */
export function waitsOnScreen(
  items: Pick<Item, "turnId" | "data">[],
  turnId: string,
  asking: readonly (string | null | undefined)[] = [],
) {
  const open = new Set<string>();
  for (const item of items) {
    if (item.turnId !== turnId) continue;
    if (item.data.type === "tool_call" && /^(?:functions\.)?mac_/i.test(item.data.name)) open.add(item.data.call_id);
    else if (item.data.type === "tool_result") open.delete(item.data.call_id);
  }
  for (const call of asking) if (call) open.delete(call);
  return open.size > 0;
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
  "Always allow {app}",
  ...Object.values(actionLines),
  ...Object.values(scrollLines),
  "“{label}”",
  "the marked spot",
  "Open {app}",
  "Quit {app}",
  "Press {keys} in {app}",
  "Drag {target} in {app}",
  "Drag {target} from {app} to {endApp}",
  "Agents won't ask before using {app} again, in any chat. You can change this in Settings → Computer use.",
  "Look at every app's windows",
  "Everything open on screen, apart from password managers and Haarness, goes to the model.",
  "Terminal and editor text can hold someone's prompt, so every click on it needs your approval.",
  "You set this app to ask every time.",
];

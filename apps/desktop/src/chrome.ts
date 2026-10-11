/**
 * Haarness for Chrome, as Desktop sees it: the server's `browser.chrome`
 * settings, the hosts API, the `haarness browser setup --json` report, and
 * the explicit Chrome approvals. Shared by main and renderer, so it returns
 * data and English-free kinds; the renderer chooses the words.
 */
import type { BrowserDecision } from "./browser-settings";

export const CHROME_CAPABILITY = "browser-extension.v1";
export type ChromeField = "browsing" | "downloads" | "uploads" | "scripts";
export const chromeFields: readonly ChromeField[] = [
  "browsing",
  "downloads",
  "uploads",
  "scripts",
];
export type ChromeAccess = Record<ChromeField, BrowserDecision>;
export interface ChromeSite extends ChromeAccess {
  origin: string;
}
export type ChromeDefaultBrowser = "auto" | "haarness" | "chrome";
/** `browser.chrome`: always written whole, to the global layer. */
export interface ChromeSettings {
  enabled: boolean;
  defaults: ChromeAccess;
  defaultBrowser: ChromeDefaultBrowser;
  sites: ChromeSite[];
}
/** One way a project tightens Chrome. */
export type ChromeRestriction =
  | { kind: "off" }
  | { kind: "default"; category: ChromeField; decision: BrowserDecision }
  | { kind: "site"; origin: string; category: ChromeField; decision: BrowserDecision };
/** The `chrome` part of a `browser-settings` get. */
export interface ChromeSettingsView {
  settings: ChromeSettings;
  globalSettings: ChromeSettings | null;
  /** The same list as English sentences, for clients that cannot word it. */
  projectRestrictions: string[];
  /** Absent from servers that only send `projectRestrictions`. */
  restrictions?: ChromeRestriction[];
}
/** A project restriction to show: worded by the renderer, or an older
 * server's English sentence as it came. */
export type ChromeRestrictionLine = ChromeRestriction | { kind: "text"; text: string };
/** What this project tightened. A kind this Desktop does not know yet is
 * left out rather than shown half-worded. */
export function chromeRestrictions(view?: ChromeSettingsView | null): ChromeRestrictionLine[] {
  if (!view) return [];
  if (Array.isArray(view.restrictions))
    return view.restrictions.filter(
      (item) =>
        item?.kind === "off" ||
        ((item?.kind === "default" || (item?.kind === "site" && typeof item.origin === "string")) &&
          typeof item.category === "string" &&
          typeof item.decision === "string"),
    );
  return (view.projectRestrictions ?? [])
    .filter((text) => typeof text === "string" && text.trim())
    .map((text) => ({ kind: "text" as const, text }));
}
/** The restriction that turns Chrome off for the whole project, in either
 * form a server sends. */
export function turnsChromeOff(line: ChromeRestrictionLine) {
  return (
    line.kind === "off" ||
    (line.kind === "text" && line.text.replace(/\.$/, "") === "Chrome is turned off for this project")
  );
}
export function defaultChromeSettings(): ChromeSettings {
  return {
    enabled: true,
    defaults: { browsing: "ask", downloads: "ask", uploads: "ask", scripts: "ask" },
    defaultBrowser: "auto",
    sites: [],
  };
}
/**
 * What the Chrome rows edit. A project can only tighten Chrome, so the rows
 * show and save the global layer; the effective value would write a
 * project's restrictions into every project on this computer.
 */
export function editableChromeSettings(view?: ChromeSettingsView | null) {
  return structuredClone(view?.globalSettings ?? defaultChromeSettings());
}

/** One connected browser from `GET /v1/browser/hosts`. */
export interface ChromeHost {
  kind: string;
  browserId: string;
  name: string;
  label?: string | null;
  version?: string;
  extensionVersion?: string;
  paused: boolean;
  stopped: boolean;
  connectedAt?: string;
  threads?: number;
  tabs?: number;
  /** Tabs held for the thread named in the request, when one was. */
  threadTabs?: number;
}
export interface ChromeHosts {
  hosts: ChromeHost[];
  chrome?: ChromeAccessReport;
}
/** How agents may use Chrome, as the hosts API reports it. */
export interface ChromeAccessReport {
  /** Whether agents may use Chrome in this project, all things considered. */
  enabled: boolean;
  /** "Let agents use Chrome", shared by every project. Older servers leave
   * it out. */
  globalEnabled?: boolean;
  /** What keeps Chrome off although that switch is on. */
  blockedBy?: "browser-control" | "project" | null;
  defaultBrowser: ChromeDefaultBrowser;
  everConnected: boolean;
}
/**
 * What keeps Chrome off in this project although "Let agents use Chrome" is
 * on: agent browser control, or the project's own settings. `asked` is the
 * switch as it was when the server was asked; until the server has seen the
 * switch as it is now, there is nothing to explain yet. An older server only
 * says Chrome is off, so its restrictions tell the two apart.
 */
export function chromeBlockedBy(
  access: (ChromeAccessReport & { asked?: boolean }) | undefined,
  enabled: boolean,
  restrictions: readonly ChromeRestrictionLine[],
): "browser-control" | "project" | undefined {
  if (!access || access.enabled || !enabled) return undefined;
  if ((access.globalEnabled ?? access.asked) !== true) return undefined;
  if (access.blockedBy === "browser-control" || access.blockedBy === "project")
    return access.blockedBy;
  return restrictions.some(turnsChromeOff) ? "project" : "browser-control";
}
export function chromeHostName(host: Pick<ChromeHost, "name" | "label">) {
  return host.label ? `${host.name} (${host.label})` : host.name;
}
export type ChromeConnection =
  | { state: "none" }
  | { state: "connected"; name: string }
  | { state: "several"; count: number }
  | { state: "paused" }
  | { state: "stopped" };
/**
 * One line for every connected Chrome: a browser the user paused or stopped
 * counts only when no other one is usable, and then says which it was.
 */
export function chromeConnection(hosts: readonly ChromeHost[]): ChromeConnection {
  const chrome = hosts.filter((host) => host.kind === "chrome");
  if (!chrome.length) return { state: "none" };
  const usable = chrome.filter((host) => !host.paused && !host.stopped);
  if (usable.length === 1)
    return { state: "connected", name: chromeHostName(usable[0]) };
  if (usable.length > 1) return { state: "several", count: usable.length };
  return chrome.every((host) => host.stopped)
    ? { state: "stopped" }
    : { state: "paused" };
}
/** How many of one chat's tabs live in Chrome, across every connected browser. */
export function chromeThreadTabs(hosts: readonly ChromeHost[]) {
  return hosts
    .filter((host) => host.kind === "chrome")
    .reduce((sum, host) => sum + Math.max(0, host.threadTabs ?? 0), 0);
}

/** One browser in the `haarness browser setup|status --json` report. */
export interface ChromeSetupBrowser {
  browser: string;
  name: string;
  extensionsPage?: string;
  manifest?: string | null;
  detected?: boolean;
  registered?: boolean;
  supported?: boolean;
  note?: string | null;
}
export interface ChromeSetupInfo {
  hostName: string;
  extensionId: string;
  webStoreUrl?: string | null;
  executable?: string;
  wrapper?: string;
  removed?: boolean;
  warnings?: string[];
  browsers: ChromeSetupBrowser[];
  extension?: { path: string; version?: string } | null;
}
/** Browsers Chrome-side setup reached, and the ones it could only explain. */
export function setupBrowsers(info: ChromeSetupInfo) {
  return {
    ready: info.browsers.filter(
      (item) => item.registered && item.supported !== false,
    ),
    unsupported: info.browsers.filter((item) => item.supported === false),
  };
}
/** The extensions page to open: Google Chrome's when it is set up. */
export function extensionsPage(info: ChromeSetupInfo) {
  const { ready } = setupBrowsers(info);
  const preferred =
    ready.find((item) => item.browser === "chrome") ?? ready[0];
  return preferred?.extensionsPage || "chrome://extensions";
}
/** The CLI's English note that this installation has no Haarness for
 * Chrome to add. The dialog says that itself, translated. */
export function notIncludedWarning(warning: string) {
  return warning.includes("not included in this installation");
}
/**
 * Whether Desktop may refresh Chrome's registration at launch. Only an
 * installed Desktop on its usual data does: a test run, or one pointed at
 * another data folder (`HAARNESS_DATA_DIR` or `HAARNESS_HOME`, which the CLI
 * also treats as isolated), would point the user's real Chrome at that
 * folder.
 */
export function chromeLaunchSetupAllowed(
  packaged: boolean,
  env: Readonly<Record<string, string | undefined>>,
) {
  return (
    packaged &&
    !["HAARNESS_DESKTOP_TEST", "HAARNESS_DATA_DIR", "HAARNESS_DESKTOP_DATA", "HAARNESS_HOME"].some(
      (name) => !!env[name],
    )
  );
}
/** Only an https Web Store link is offered; anything else falls back to the
 * unpacked-extension steps. */
export function webStoreLink(info: ChromeSetupInfo) {
  try {
    const url = new URL(info.webStoreUrl ?? "");
    return url.protocol === "https:" ? url.href : undefined;
  } catch {
    return undefined;
  }
}

/**
 * A Chrome site gate: `chrome-<category>:<thread>:<origin>`, prefixed with
 * `browser_site:` when it is combined with other browser permissions.
 */
const chromePattern =
  /^(?:browser_site:)?chrome-(browsing|downloads|uploads|scripts):([^:\s]+):(\S+)$/;
export function chromeApproval(pattern: string) {
  const match = chromePattern.exec(pattern);
  return match
    ? { category: match[1] as ChromeField, thread: match[2], origin: match[3] }
    : undefined;
}
/**
 * A request that only a person may answer. The server marks it `explicit`;
 * a Chrome site pattern is treated the same way, so a client never answers
 * one on the user's behalf even before the server says so.
 */
export function isExplicitPermission(request: {
  explicit?: boolean | null;
  operation?: string;
  patterns?: readonly string[];
}) {
  return (
    request.explicit === true ||
    !!(request.operation && chromeApproval(request.operation)) ||
    (request.patterns ?? []).some((pattern) => !!chromeApproval(pattern))
  );
}
const wwwTwin = (origin: string) => {
  try {
    const url = new URL(origin);
    url.hostname = url.hostname.startsWith("www.")
      ? url.hostname.slice(4)
      : "www." + url.hostname;
    return url.origin;
  } catch {
    return origin;
  }
};
/**
 * What an explicit Chrome request asks for, one line per category and site.
 * A site and its `www.` twin are asked together, so they read as one line.
 */
export function chromeApprovalLines(patterns: readonly string[]) {
  const lines: { category: ChromeField; origin: string }[] = [];
  for (const pattern of patterns) {
    const approval = chromeApproval(pattern);
    if (
      !approval ||
      lines.some(
        (line) =>
          line.category === approval.category &&
          (line.origin === approval.origin ||
            line.origin === wwwTwin(approval.origin)),
      )
    )
      continue;
    lines.push({ category: approval.category, origin: approval.origin });
  }
  return lines;
}

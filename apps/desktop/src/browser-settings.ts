/** Shared browser settings contract. Access rules are stored on the project server. */
export type BrowserDecision = "allow" | "ask" | "deny";
export interface BrowserAccess {
  browsing: BrowserDecision;
  downloads: BrowserDecision;
  uploads: BrowserDecision;
}
export interface BrowserSite extends BrowserAccess {
  origin: string;
}
export interface BrowserPolicy {
  enabled: boolean;
  rememberApprovals?: boolean;
  defaults: BrowserAccess;
  sites: BrowserSite[];
}
export interface BrowserSettingsValue {
  policy: BrowserPolicy;
  scopePolicy: BrowserPolicy | null;
  inheritedPolicy: BrowserPolicy;
  source: string;
  scope: "project" | "global";
  revision: string;
}
export interface BrowserVisit {
  id: string;
  thread: string;
  url: string;
  title: string;
  time: string;
}
export interface BrowserDownload {
  id: string;
  thread: string;
  url: string;
  name: string;
  bytes: number;
  time: string;
  state: "completed" | "failed";
}
export interface BrowserData {
  history: BrowserVisit[];
  downloads: BrowserDownload[];
}
export function isLocalBrowserURL(raw: string) {
  try {
    const host = new URL(raw).hostname.toLowerCase();
    return (
      host === "localhost" ||
      host.endsWith(".localhost") ||
      host === "[::1]" ||
      /^127(?:\.\d{1,3}){3}$/.test(host)
    );
  } catch {
    return false;
  }
}
export function browserLinkDestination(
  url: string,
  preferences: {
    browserWebLinks: "external" | "browser";
    browserLocalLinks: "external" | "browser";
  },
) {
  if (!/^https?:\/\//i.test(url)) return "external";
  return isLocalBrowserURL(url)
    ? preferences.browserLocalLinks
    : preferences.browserWebLinks;
}
export function browserAddress(url: string, full: boolean) {
  if (full) return url;
  try {
    const parsed = new URL(url);
    return /^https?:$/.test(parsed.protocol) ? parsed.origin : url;
  } catch {
    return url;
  }
}

/** Exact origins take priority; otherwise the most specific subdomain rule wins. Shared with the browser driver. */
export { browserSiteAccess } from "@haarness/browser-driver/policy";

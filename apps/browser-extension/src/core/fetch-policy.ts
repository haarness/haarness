/**
 * Site rules applied inside the page while an agent command runs.
 *
 * The server gates every command on the origins it targets, but a page can
 * still go elsewhere on its own: a redirect, a script navigation, a form. While
 * a command runs the extension pauses document requests (and every request to
 * a blocked origin) with CDP `Fetch`, and decides each hop here:
 *   - a blocked (Deny) origin is refused for every resource type;
 *   - the tab's own page may only load an Ask origin this chat has approved;
 *   - the tab's own page may not leave the web (file:, chrome:, data:, …).
 * Everything else continues, so ordinary pages are not stalled. Only a
 * refused page of the tab's own fails the command; a refused frame or
 * resource is just not loaded. After the command (and for main-frame
 * navigations within 2 s of it) a backstop checks where the tab landed with
 * `landing()`.
 *
 * Not covered: requests from workers and service workers, and requests an
 * out-of-process frame starts before `Fetch.enable` reaches it; the backstop
 * still catches a top-level page that ends up somewhere it may not be.
 */
import type { Policy } from "./browser";
import { destinationOrigin, siteAccess } from "./policy";

export interface FetchContext {
  policy: Policy;
  /** Origins this chat's commands were approved for. */
  approved: ReadonlySet<string>;
}

export type FetchDecision =
  | { action: "continue" }
  | { action: "block"; reason: "deny" | "ask" | "scheme" | "disabled"; origin: string };

export interface RequestPattern {
  urlPattern?: string;
  resourceType?: "Document";
}

/** What to pause: every document, plus everything to a blocked origin. */
export function fetchPatterns(policy: Policy): RequestPattern[] {
  if (!policy.enabled || policy.defaults.browsing === "deny") return [{ urlPattern: "*" }];
  return [
    { resourceType: "Document" },
    ...policy.sites
      .filter((site) => site.browsing === "deny")
      .map((site) => ({ urlPattern: patternFor(site.origin) })),
  ];
}

/** `https://*.example.com` → `https://*.example.com/*`; over-matching is fine, `decide` re-checks. */
function patternFor(origin: string) {
  return origin.replace(/[\\?]/g, (char) => "\\" + char) + "/*";
}

export function decideRequest(
  context: FetchContext,
  request: { url: string; resourceType: string; mainFrame: boolean },
): FetchDecision {
  const origin = destinationOrigin(request.url);
  const page = request.mainFrame && request.resourceType === "Document";
  if (!origin)
    return page ? { action: "block", reason: "scheme", origin: scheme(request.url) } : { action: "continue" };
  if (!context.policy.enabled) return { action: "block", reason: "disabled", origin };
  const browsing = siteAccess(context.policy, origin).browsing;
  if (browsing === "deny") return { action: "block", reason: "deny", origin };
  if (page && browsing === "ask" && !context.approved.has(origin)) return { action: "block", reason: "ask", origin };
  return { action: "continue" };
}

/** Where a tab may rest after a command: a blank page or an allowed top-level origin. */
export function landing(context: FetchContext, url: string): FetchDecision {
  // Chrome's own error page for a load that failed: the failure is reported, not blocked.
  if (url === "" || url === "about:blank" || url.startsWith("chrome-error:")) return { action: "continue" };
  return decideRequest(context, { url, resourceType: "Document", mainFrame: true });
}

function scheme(url: string) {
  const match = /^([a-z][a-z\d+.-]*):/i.exec(url);
  return match ? match[1].toLowerCase() + ":" : url.slice(0, 32);
}

/** The model-facing reason, with code `site-blocked`. */
export function blockedMessage(decision: Extract<FetchDecision, { action: "block" }>): string {
  switch (decision.reason) {
    case "ask":
      return `Chrome stopped the page from opening ${decision.origin}, which this chat has not been allowed to use. Call browser_navigate with that URL to ask the user, or choose another site.`;
    case "deny":
      return `Chrome blocked ${decision.origin}: the user does not let agents use this site. Choose another site.`;
    case "scheme":
      return `Chrome stopped the page from opening a ${decision.origin} address; agents can use only http(s) sites and about:blank. Choose another site.`;
    case "disabled":
      return `Chrome blocked ${decision.origin}: agents may not use Chrome right now.`;
  }
}

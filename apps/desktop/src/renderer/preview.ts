// Shows HTML that a model wrote, in the file panel and in the conversation,
// without handing it the app. See docs/CANVAS-PREVIEW.md.
//
// The page runs two frames deep. The outer frame is an opaque document whose
// Content-Security-Policy decides where the page may navigate; the inner one
// holds the page itself under a second policy that refuses every request
// except to one public npm CDN. Links to other well-known CDNs are pointed at
// that one before the page loads, so common libraries keep working.
export namespace Preview {
  /** The only origin a previewed page may load scripts, styles, fonts and images from. */
  export const MIRROR = "https://unpkg.com";

  export type Kind = "html" | "markdown" | "svg" | "image";

  /** One CDN link pointed at the mirror. `exact` is false when the mirror's file layout may differ. */
  export type Replacement = { from: string; to: string; exact: boolean };

  export type Report = {
    html: string;
    replaced: Replacement[];
    /** Remote addresses the page names that the policy will refuse. */
    blocked: string[];
  };

  /** What a previewed page reports to the app. */
  export type Message =
    | { type: "ready" }
    | { type: "navigation" }
    | { type: "size"; height: number; width: number }
    | { type: "error"; message: string }
    | { type: "blocked"; url: string; directive: string }
    /** A button was let go over the page; the app's window is not told. */
    | { type: "lift" };

  const KINDS = new Map<string, Kind>([
    ["html", "html"],
    ["htm", "html"],
    ["svg", "svg"],
    ...["md", "markdown", "mdx", "mdown", "mkd", "mkdn"].map((ext): [string, Kind] => [ext, "markdown"]),
    ...["png", "jpg", "jpeg", "gif", "webp", "avif", "bmp", "ico"].map((ext): [string, Kind] => [ext, "image"]),
  ]);

  /** How a file shows in Preview, from its extension; undefined when it has no preview. */
  export function kind(path: string): Kind | undefined {
    const dot = path.lastIndexOf(".");
    return dot < 0 ? undefined : KINDS.get(path.slice(dot + 1).toLowerCase());
  }

  // The rest of a URL: it stops at white space, a quote, an angle bracket, a
  // parenthesis or a backtick.
  const URL_REST = String.raw`[^\s"'<>()\x60]+`;
  const SEGMENT = String.raw`[^/\s"'<>]+`;
  const SCHEME = String.raw`(?:https?:)?//`;

  type Rule = { pattern: RegExp; target: (parts: Record<string, string | undefined>) => [path: string, exact: boolean] };

  // Each rule names a public CDN layout and the matching `package@version/file`
  // path on the mirror.
  const RULES: Rule[] = [
    {
      pattern: new RegExp(`${SCHEME}cdn\\.jsdelivr\\.net/npm/(?<rest>${URL_REST})`, "g"),
      target: ({ rest }) => [rest!, true],
    },
    {
      // Already on the mirror: only the scheme is made explicit.
      pattern: new RegExp(`${SCHEME}unpkg\\.com/(?<rest>${URL_REST})`, "g"),
      target: ({ rest }) => [rest!, true],
    },
    {
      // cdnjs folders are named after the library, which npm spells in lower
      // case, and its file layout is its own: the mirror path is a best guess.
      pattern: new RegExp(
        `${SCHEME}cdnjs\\.cloudflare\\.com/ajax/libs/(?<library>${SEGMENT})/(?<version>${SEGMENT})/(?<file>${URL_REST})`,
        "g",
      ),
      target: ({ library, version, file }) => [`${library!.toLowerCase()}@${version}/${file}`, false],
    },
    {
      // d3js.org/d3.v7.min.js is d3@7/dist/d3.min.js.
      pattern: new RegExp(`${SCHEME}d3js\\.org/d3(?:\\.v(?<major>\\d+))?(?<min>\\.min)?\\.js`, "g"),
      target: ({ major, min }) => [`d3@${major ?? "7"}/dist/d3${min ?? ""}.js`, true],
    },
  ];

  /** Points known CDN links at the mirror and lists the remote addresses that stay blocked. */
  export function rewrite(html: string, mirror = MIRROR): Report {
    const replaced: Replacement[] = [];
    let text = html;
    for (const rule of RULES) {
      text = text.replace(rule.pattern, (...args: unknown[]) => {
        const from = args[0] as string;
        const parts = args[args.length - 1] as Record<string, string | undefined>;
        const [path, exact] = rule.target(parts);
        const to = `${mirror}/${path}`;
        if (to !== from) replaced.push({ from, to, exact });
        return to;
      });
    }
    return { html: text, replaced, blocked: blocked(text, mirror) };
  }

  // Places in a page where a remote address makes the browser fetch it.
  const FETCHES = [
    String.raw`<(?:script|link|img|iframe|source|video|audio|embed|object)\b[^>]*?\b(?:src|href|data)\s*=\s*["']?`,
    String.raw`url\(\s*["']?`,
    String.raw`@import\s+["']`,
    String.raw`\bfrom\s+["']`,
    String.raw`\bimport\(\s*["']`,
    String.raw`\bfetch\(\s*["']`,
  ].map((lead) => new RegExp(`${lead}(${SCHEME}${URL_REST})`, "gi"));

  function hostOf(address: string): string | undefined {
    try {
      return new URL(address, "https://page.invalid").host;
    } catch {
      return undefined;
    }
  }

  /** Remote addresses in the page that are not on the mirror, each once, in the order found. */
  export function blocked(html: string, mirror = MIRROR): string[] {
    const allowed = new URL(mirror).host;
    const found = new Set<string>();
    for (const pattern of FETCHES) for (const match of html.matchAll(pattern)) found.add(match[1]);
    return [...found].filter((address) => hostOf(address) !== allowed);
  }

  /** The page's Content-Security-Policy: inline code and the mirror, and nothing else. */
  export function policy(mirror = MIRROR): string {
    const origin = new URL(mirror).origin;
    const directives: [string, string][] = [
      ["default-src", "'none'"],
      ["script-src", `'unsafe-inline' 'unsafe-eval' blob: ${origin}`],
      ["style-src", `'unsafe-inline' ${origin}`],
      ["font-src", `data: ${origin}`],
      ["img-src", `data: blob: ${origin}`],
      ["media-src", "data: blob:"],
      ["connect-src", "'none'"],
      ["frame-src", "'none'"],
      ["worker-src", "blob:"],
      ["form-action", "'none'"],
      ["base-uri", "'none'"],
    ];
    return directives.map(([name, sources]) => `${name} ${sources}`).join("; ");
  }

  // Runs inside the page. It reports the page's size as it changes, that it
  // loaded, its errors and the requests the policy refused, each tagged with
  // the preview id; it keeps in-page links working and turns every other link
  // into a "navigation" report. The outer frame's policy is what actually stops
  // navigation by script or meta refresh; this only makes clicks behave. It
  // also reports a button let go over the page, which ends a press held in the
  // conversation (transcript-press.ts) that the app's window never sees end.
  function reporter(previewId: string) {
    const body = `
var previewId = ${JSON.stringify(previewId).replace(/</g, "\\u003c")};
function report(message) { message.preview = previewId; try { parent.postMessage(message, "*"); } catch (_) {} }
function reportSize() {
  var root = document.documentElement, body = document.body;
  report({ type: "size",
    height: Math.max(root.scrollHeight, body ? body.scrollHeight : 0),
    width: Math.max(root.scrollWidth, body ? body.scrollWidth : 0) });
}
var watcher = typeof ResizeObserver === "function" ? new ResizeObserver(reportSize) : null;
addEventListener("DOMContentLoaded", function () { if (watcher && document.body) watcher.observe(document.body); reportSize(); });
addEventListener("load", function () { reportSize(); report({ type: "ready" }); });
addEventListener("click", function (event) {
  var link = event.target && event.target.closest ? event.target.closest("a[href],area[href]") : null;
  if (!link) return;
  var href = link.getAttribute("href") || "";
  if (href.charAt(0) === "#") {
    event.preventDefault();
    var name = href.slice(1);
    try { name = decodeURIComponent(name); } catch (_) {}
    var target = document.getElementById(name) || document.getElementsByName(name)[0];
    if (target) target.scrollIntoView();
    else if (name === "" || name.toLowerCase() === "top") scrollTo(0, 0);
    return;
  }
  if (link.hasAttribute("download") && /^(?:blob|data):/i.test(href)) return;
  event.preventDefault();
  report({ type: "navigation" });
}, true);
addEventListener("error", function (event) {
  var source = event.target;
  report({ type: "error", message: String(event.message || (source && (source.src || source.href)) || "Resource failed to load") });
}, true);
addEventListener("unhandledrejection", function (event) {
  var reason = event.reason;
  report({ type: "error", message: String(reason && reason.message || reason) });
});
addEventListener("securitypolicyviolation", function (event) {
  report({ type: "blocked", url: String(event.blockedURI), directive: String(event.violatedDirective) });
});
function lift(event) { if (event.isTrusted) report({ type: "lift" }); }
addEventListener("pointerup", lift, true);
addEventListener("pointercancel", lift, true);`;
    return `<script data-haarness-preview>(function () {${body}\n})();</script>`;
  }

  const DOCTYPE = /^\s*<!doctype[^>]*>/i;

  /**
   * The page with its CDN links rewritten and the policy and reporter put in
   * front of everything it contains, so the policy covers all of it. They go
   * after the doctype: anything before it would put the page in quirks mode.
   */
  export function document(input: string, options: { id: string; mirror?: string }): Report {
    const report = rewrite(input, options.mirror);
    const doctype = report.html.match(DOCTYPE)?.[0] ?? "<!doctype html>";
    const page = report.html.replace(DOCTYPE, "");
    const guard =
      `<meta data-haarness-preview http-equiv="Content-Security-Policy" content="${policy(options.mirror)}">` +
      reporter(options.id);
    return { ...report, html: doctype + guard + page };
  }

  function attribute(text: string): string {
    return text.replace(/&/g, "&amp;").replace(/"/g, "&quot;").replace(/</g, "&lt;");
  }

  /**
   * The outer frame around the page. A document's own policy cannot stop it
   * from navigating, but the policy of the frame that holds it can: frame-src
   * is checked before a child frame navigates. So the page sits in a frame of
   * this opaque document, whose frame-src allows only blob: and data: (local
   * downloads, which WebKit also checks against it). It passes the page's
   * reports up to the app and turns a refused navigation into one.
   */
  export function frame(input: string, options: { id: string; mirror?: string }): Report {
    const page = document(input, options);
    const outerPolicy = policy(options.mirror).replace("frame-src 'none'", "frame-src blob: data:");
    const previewId = JSON.stringify(options.id).replace(/</g, "\\u003c");
    const relay = `<script>(function () {
var previewId = ${previewId};
var kinds = ["ready", "size", "error", "blocked", "navigation", "lift"];
function up(message) { if (parent === window) return; message.preview = previewId; parent.postMessage(message, "*"); }
addEventListener("message", function (event) {
  var inner = document.getElementById("page"), data = event.data;
  if (!inner || !data || data.preview !== previewId) return;
  if (event.source === inner.contentWindow && kinds.indexOf(data.type) >= 0) up(data);
});
addEventListener("securitypolicyviolation", function (event) {
  up({ type: "blocked", url: String(event.blockedURI), directive: String(event.violatedDirective) });
  if (event.violatedDirective === "frame-src") up({ type: "navigation" });
});
function lift(event) { if (event.isTrusted) up({ type: "lift" }); }
addEventListener("pointerup", lift, true);
addEventListener("pointercancel", lift, true);
})();</script>`;
    const style = "html,body,iframe{margin:0;border:0;width:100%;height:100%;display:block}html,body{overflow:hidden}";
    const html =
      `<!doctype html><html><head><meta http-equiv="Content-Security-Policy" content="${outerPolicy}">` +
      `<title>Preview</title><style>${style}</style></head><body>\n${relay}` +
      `<iframe id="page" title="Page" sandbox="allow-scripts allow-downloads" referrerpolicy="no-referrer" srcdoc="${attribute(page.html)}"></iframe>` +
      `</body></html>`;
    return { ...page, html };
  }

  /** A report from the preview with this id, or undefined for anything else. */
  export function message(data: unknown, id: string): Message | undefined {
    if (typeof data !== "object" || data === null) return undefined;
    const value = data as Record<string, unknown>;
    if (value.preview !== id) return undefined;
    switch (value.type) {
      case "ready":
      case "navigation":
      case "lift":
        return { type: value.type };
      case "size":
        if (typeof value.height !== "number") return undefined;
        return { type: "size", height: value.height, width: typeof value.width === "number" ? value.width : 0 };
      case "error":
        return { type: "error", message: String(value.message) };
      case "blocked":
        return { type: "blocked", url: String(value.url), directive: String(value.directive) };
      default:
        return undefined;
    }
  }
}

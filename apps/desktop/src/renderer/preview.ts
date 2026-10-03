// Adapted from Upstream (MIT). See public/upstream-preview-license.txt.
// Safe rendering of model-written HTML in the file panel and conversation.
// Two layers (see docs/CANVAS-PREVIEW.md):
// - rewrite: known public CDN references are pointed at the public npm CDN so common libraries work.
// - document: a CSP <meta> and a sandboxed iframe make every other request fail closed.
export namespace Preview {
  export const MIRROR = "https://unpkg.com";

  export type Kind = "html" | "markdown" | "svg" | "image";

  export type Replacement = { from: string; to: string; exact: boolean };

  export type Report = {
    html: string;
    replaced: Replacement[];
    blocked: string[];
  };

  export type Message =
    | { type: "ready" }
    | { type: "navigation" }
    | { type: "size"; height: number; width: number }
    | { type: "error"; message: string }
    | { type: "blocked"; url: string; directive: string }
    | { type: "snapshot"; html: string };

  const IMAGES = new Set([
    "png",
    "jpg",
    "jpeg",
    "gif",
    "webp",
    "avif",
    "bmp",
    "ico",
  ]);
  const MARKDOWN = new Set(["md", "markdown", "mdx", "mdown", "mkd", "mkdn"]);

  export function kind(path: string): Kind | undefined {
    const ext = path.split(".").pop()?.toLowerCase() ?? "";
    if (ext === "html" || ext === "htm") return "html";
    if (ext === "svg") return "svg";
    if (MARKDOWN.has(ext)) return "markdown";
    if (IMAGES.has(ext)) return "image";
    return undefined;
  }

  const TAIL = `[^\\s"'<>()\`]+`;

  // Each rule maps a public CDN URL to the mirror's `pkg@version/path` layout.
  // `exact` is false when the source layout doesn't map one-to-one and the target may 404.
  const RULES: {
    pattern: RegExp;
    map: (...parts: string[]) => { path: string; exact: boolean };
  }[] = [
    {
      pattern: new RegExp(
        `(?:https?:)?//cdn\\.jsdelivr\\.net/npm/(${TAIL})`,
        "g",
      ),
      map: (rest) => ({ path: rest, exact: true }),
    },
    {
      pattern: new RegExp(`(?:https?:)?//unpkg\\.com/(${TAIL})`, "g"),
      map: (rest) => ({ path: rest, exact: true }),
    },
    {
      // cdnjs: /ajax/libs/<lib>/<version>/<file>; npm names are lowercase and dist folders differ.
      pattern: new RegExp(
        `(?:https?:)?//cdnjs\\.cloudflare\\.com/ajax/libs/([^/\\s"'<>]+)/([^/\\s"'<>]+)/(${TAIL})`,
        "g",
      ),
      map: (lib, version, file) => ({
        path: `${lib.toLowerCase()}@${version}/${file}`,
        exact: false,
      }),
    },
    {
      // d3js.org/d3.v7.min.js -> d3@7/dist/d3.min.js
      pattern: /(?:https?:)?\/\/d3js\.org\/d3(?:\.v(\d+))?(\.min)?\.js/g,
      map: (major, min) => ({
        path: `d3@${major || "7"}/dist/d3${min ?? ""}.js`,
        exact: true,
      }),
    },
  ];

  export function rewrite(html: string, mirror = MIRROR): Report {
    const replaced: Replacement[] = [];
    const out = RULES.reduce(
      (text, rule) =>
        text.replace(rule.pattern, (from: string, ...rest: unknown[]) => {
          // Callback args are (match, ...groups, offset, input); groups end at the numeric offset.
          const groups = rest.slice(
            0,
            rest.findIndex((part) => typeof part === "number"),
          );
          const target = rule.map(
            ...groups.map((part) => (typeof part === "string" ? part : "")),
          );
          const to = `${mirror}/${target.path}`;
          if (from !== to) replaced.push({ from, to, exact: target.exact });
          return to;
        }),
      html,
    );
    return { html: out, replaced, blocked: blocked(out, mirror) };
  }

  // Remote subresources that the CSP will block (anything not on the mirror).
  export function blocked(html: string, mirror = MIRROR) {
    const host = new URL(mirror).host;
    const url = `((?:https?:)?//[^\\s"'<>()\`]+)`;
    const patterns = [
      new RegExp(
        `<(?:script|link|img|iframe|source|video|audio|embed|object)\\b[^>]*?\\b(?:src|href|data)\\s*=\\s*["']?${url}`,
        "gi",
      ),
      new RegExp(`url\\(\\s*["']?${url}`, "gi"),
      new RegExp(`@import\\s+["']${url}`, "gi"),
      new RegExp(`\\bfrom\\s+["']${url}`, "gi"),
      new RegExp(`\\bimport\\(\\s*["']${url}`, "gi"),
      new RegExp(`\\bfetch\\(\\s*["']${url}`, "gi"),
    ];
    const found = patterns.flatMap((pattern) =>
      Array.from(html.matchAll(pattern), (match) => match[1]),
    );
    return Array.from(new Set(found)).filter((item) => {
      const parsed = URL.canParse(item, "https://x")
        ? new URL(item, "https://x")
        : undefined;
      return parsed?.host !== host;
    });
  }

  export function policy(mirror = MIRROR) {
    const origin = new URL(mirror).origin;
    return [
      "default-src 'none'",
      `script-src 'unsafe-inline' 'unsafe-eval' blob: ${origin}`,
      `style-src 'unsafe-inline' ${origin}`,
      `font-src data: ${origin}`,
      `img-src data: blob: ${origin}`,
      "media-src data: blob:",
      "connect-src 'none'",
      "frame-src 'none'",
      "worker-src blob:",
      "form-action 'none'",
      "base-uri 'none'",
    ].join("; ");
  }

  // Serializes the page as rendered, for email: canvases become images, form state becomes
  // attributes, and scripts, handlers, javascript: URLs and our injected tags are removed.
  export const SNAPSHOT = `function snap(){var root=document.documentElement.cloneNode(true),i,j;
var from=document.querySelectorAll("canvas"),to=root.querySelectorAll("canvas");
for(i=0;i<from.length&&i<to.length;i++){try{var c=from[i],r=c.getBoundingClientRect(),img=document.createElement("img");img.src=c.toDataURL("image/png");if(c.id)img.id=c.id;if(c.getAttribute("class"))img.setAttribute("class",c.getAttribute("class"));if(c.getAttribute("style"))img.setAttribute("style",c.getAttribute("style"));img.style.width=r.width+"px";img.style.height=r.height+"px";img.setAttribute("width",String(Math.round(r.width)));img.setAttribute("height",String(Math.round(r.height)));img.setAttribute("alt",c.getAttribute("aria-label")||"");to[i].parentNode.replaceChild(img,to[i])}catch(e){}}
var fi=document.querySelectorAll("input,textarea,select"),ti=root.querySelectorAll("input,textarea,select");
for(i=0;i<fi.length&&i<ti.length;i++){var a=fi[i],b=ti[i];if(a.tagName==="TEXTAREA")b.textContent=a.value;else if(a.tagName==="SELECT"){for(j=0;j<a.options.length&&j<b.options.length;j++){if(a.options[j].selected)b.options[j].setAttribute("selected","");else b.options[j].removeAttribute("selected")}}else if(a.type==="checkbox"||a.type==="radio"){if(a.checked)b.setAttribute("checked","");else b.removeAttribute("checked")}else b.setAttribute("value",a.value)}
var drop=root.querySelectorAll("script,noscript,template,[data-haarness-preview]");
for(i=0;i<drop.length;i++)if(drop[i].parentNode)drop[i].parentNode.removeChild(drop[i]);
var all=root.querySelectorAll("*");
for(i=0;i<all.length;i++){var at=all[i].attributes;for(j=at.length-1;j>=0;j--){var n=at[j].name.toLowerCase(),v=at[j].value.trim().toLowerCase();if(n.indexOf("on")===0||(v.indexOf("javascript:")===0&&(n==="href"||n==="src"||n==="action"||n==="formaction"||n==="xlink:href")))all[i].removeAttribute(at[j].name)}}
return "<!doctype html>\\n"+root.outerHTML}`;

  // Posts size, runtime errors and CSP violations to the parent frame, tagged with `id`, and
  // answers snapshot requests from the parent.
  function bridge(id: string) {
    return `<script data-haarness-preview>(function(){var id=${JSON.stringify(id)};function post(m){m.preview=id;try{parent.postMessage(m,"*")}catch(e){}}
${SNAPSHOT}
addEventListener("message",function(e){var d=e.data;if(e.source!==parent||!d||d.preview!==id||d.type!=="snapshot")return;post({type:"snapshot",html:snap()})});
function size(){var d=document.documentElement,b=document.body;post({type:"size",height:Math.max(d.scrollHeight,b?b.scrollHeight:0),width:Math.max(d.scrollWidth,b?b.scrollWidth:0)})}
var ro=typeof ResizeObserver==="function"?new ResizeObserver(size):null;
addEventListener("DOMContentLoaded",function(){if(ro&&document.body)ro.observe(document.body);size()});
addEventListener("load",function(){size();post({type:"ready"})});
// Keep ordinary links on the page. The outer frame's CSP also blocks navigation initiated
// directly by scripts or meta refresh; this handler is only a usability aid.
addEventListener("click",function(e){var a=e.target&&e.target.closest?e.target.closest("a[href],area[href]"):null;if(!a)return;var href=a.getAttribute("href")||"";if(href.charAt(0)==="#"){e.preventDefault();var name=href.slice(1);try{name=decodeURIComponent(name)}catch(e){}var target=document.getElementById(name)||document.getElementsByName(name)[0];if(target)target.scrollIntoView();else if(!name||name.toLowerCase()==="top")window.scrollTo(0,0);return}if(/^blob:|^data:/i.test(href)&&a.hasAttribute("download"))return;e.preventDefault();post({type:"navigation"})},true);
addEventListener("error",function(e){var t=e.target;post({type:"error",message:String(e.message||(t&&(t.src||t.href))||"Resource failed to load")})},true);
addEventListener("unhandledrejection",function(e){post({type:"error",message:String(e.reason&&e.reason.message||e.reason)})});
addEventListener("securitypolicyviolation",function(e){post({type:"blocked",url:String(e.blockedURI),directive:String(e.violatedDirective)})});
})()</script>`;
  }

  // Builds the srcdoc for a sandboxed iframe: CDN rewrite + CSP + bridge, injected after the doctype
  // (before it would force quirks mode) and ahead of any page content so the policy applies to all of it.
  export function document(
    input: string,
    opts: { id: string; mirror?: string },
  ) {
    const report = rewrite(input, opts.mirror);
    const head = `<meta data-haarness-preview http-equiv="Content-Security-Policy" content="${policy(opts.mirror)}">${bridge(opts.id)}`;
    const doctype =
      report.html.match(/^\s*<!doctype[^>]*>/i)?.[0] ?? "<!doctype html>";
    const rest = report.html.replace(/^\s*<!doctype[^>]*>/i, "");
    return { ...report, html: `${doctype}${head}${rest}` };
  }

  // A document's CSP cannot restrict its own navigation. Put the page in a second, opaque
  // frame: the trusted owner's frame-src policy is checked BEFORE child navigation requests.
  // srcdoc inherits this policy and still permits inline scripts and the approved CDN.
  export function frame(input: string, opts: { id: string; mirror?: string }) {
    const doc = document(input, opts);
    const html = doc.html
      .replace(/&/g, "&amp;")
      .replace(/"/g, "&quot;")
      .replace(/</g, "&lt;");
    const id = JSON.stringify(opts.id).replace(/</g, "\\u003c");
    // WebKit checks local downloads against the owner's frame policy as well. Network
    // navigation remains forbidden; the page's own policy still forbids nested frames.
    const owner = policy(opts.mirror).replace(
      "frame-src 'none'",
      "frame-src blob: data:",
    );
    return {
      ...doc,
      html: `<!doctype html><html><head><meta http-equiv="Content-Security-Policy" content="${owner}"><title>Preview</title><style>html,body,iframe{margin:0;border:0;width:100%;height:100%;display:block}html,body{overflow:hidden}</style></head><body>
<script>(function(){var id=${id};function post(m){if(parent===window)return;m.preview=id;parent.postMessage(m,"*")}
addEventListener("message",function(e){var f=document.getElementById("page"),d=e.data;if(!f||!d||d.preview!==id)return;if(e.source===parent&&d.type==="snapshot"){f.contentWindow.postMessage(d,"*");return}if(e.source===f.contentWindow&&["ready","size","error","blocked","snapshot","navigation"].indexOf(d.type)!==-1)post(d)});
addEventListener("securitypolicyviolation",function(e){post({type:"blocked",url:String(e.blockedURI),directive:String(e.violatedDirective)});if(e.violatedDirective==="frame-src")post({type:"navigation"})});
})()</script><iframe id="page" title="Page" sandbox="allow-scripts allow-downloads" referrerpolicy="no-referrer" srcdoc="${html}"></iframe></body></html>`,
    };
  }

  export function message(data: unknown, id: string): Message | undefined {
    if (!data || typeof data !== "object") return undefined;
    const value = data as Record<string, unknown>;
    if (value.preview !== id) return undefined;
    if (value.type === "ready" || value.type === "navigation")
      return { type: value.type };
    if (value.type === "size" && typeof value.height === "number")
      return {
        type: "size",
        height: value.height,
        width: typeof value.width === "number" ? value.width : 0,
      };
    if (value.type === "error")
      return { type: "error", message: String(value.message) };
    if (value.type === "blocked")
      return {
        type: "blocked",
        url: String(value.url),
        directive: String(value.directive),
      };
    if (value.type === "snapshot" && typeof value.html === "string")
      return { type: "snapshot", html: value.html };
    return undefined;
  }

  /**
   * Whether the page runs JavaScript: an executable <script> (no type, a JavaScript type or a
   * module; not JSON or templates), an inline on* handler, or a javascript: URL in an attribute.
   * Comments are ignored, so a note like `<!-- no <script> needed -->` doesn't count.
   */
  export function scripted(html: string) {
    const text = html.replace(/<!--[\s\S]*?-->/g, "");
    const scripts = Array.from(text.matchAll(/<script\b([^>]*)>/gi)).some(
      (match) => {
        const type = match[1]
          .match(/\btype\s*=\s*["']?([^"'\s>]+)/i)?.[1]
          ?.toLowerCase();
        return !type || type === "module" || /(?:java|ecma)script/.test(type);
      },
    );
    if (scripts) return true;
    return (text.match(/<[a-z][^>]*>/gi) ?? []).some(
      (tag) =>
        /\son[a-z]+\s*=/i.test(tag) ||
        /\s(?:href|src|action|formaction)\s*=\s*["']?\s*javascript:/i.test(tag),
    );
  }

  export function summary(report: Pick<Report, "replaced" | "blocked">) {
    const lines = [
      ...report.replaced.map((item) =>
        item.exact
          ? `Rewritten to mirror: ${item.from} -> ${item.to}`
          : `Rewritten to mirror, path layout differs and may 404 (prefer ${MIRROR}/PKG@VERSION/dist/...): ${item.from} -> ${item.to}`,
      ),
      ...report.blocked.map(
        (item) => `Blocked (only ${MIRROR} is allowed): ${item}`,
      ),
    ];
    return lines.join("\n");
  }
}

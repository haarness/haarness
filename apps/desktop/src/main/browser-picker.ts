// The person's element picker in a Haarness browser tab.
//
// While it is on, the element under the mouse is outlined with a small label
// (its tag and size) and a click picks it for the message instead of reaching
// the page: Shift keeps picking, Option outlines exactly what is under the
// mouse rather than the control around it, and Escape stops.
//
// The page never sees the click. Presses are refused before Chromium hands
// them to the page (`before-mouse-event`), so no listener of the page, however
// early it registered, hears a mousedown, focus or click; only the pointer's
// movement reaches it, as hover does. The outline is drawn the way the agent
// cursor is (see agent-cursor.ts in the browser driver): in an isolated world
// the page cannot reach, in a closed shadow root on a top-layer popover that
// is inert and hidden from assistive technology, so snapshots never list it.
import type { WebContents } from "electron";
import type { PickedElement, PickEnd, PickRequest } from "../browser-pick";

/** Any stable id outside Electron's own worlds and the agent cursor's (1779). */
export const PICK_WORLD = 1_780;
/** The most outer HTML a pick carries. */
export const MAX_PICKED_HTML = 2_048;
/** Presses swallowed after a pick that ended picking: the rest of a double click. */
const SWALLOW_MS = 400;

/** Installs the picker in the current document once and returns its API. */
export const pickerScript = String.raw`globalThis.__haarnessPicker ??= (() => {
  const doc = document;
  const MAX_HTML = ${MAX_PICKED_HTML};
  // What a person means by a point: the button rather than the label inside it.
  const CONTROL = "a[href],button,summary,label,select,textarea,input,[role=button],[role=link],[role=tab],[role=menuitem],[role=menuitemcheckbox],[role=menuitemradio],[role=option],[role=checkbox],[role=radio],[role=switch],[role=treeitem]";
  const NAMED_BY_TEXT = "a,button,summary,label,option,legend,caption,th,h1,h2,h3,h4,h5,h6,[role=button],[role=link],[role=tab],[role=menuitem],[role=option],[role=heading]";
  const host = doc.createElement("haarness-element-picker");
  host.setAttribute("aria-hidden", "true");
  host.inert = true;
  if ("popover" in host) host.popover = "manual";
  for (const [name, value] of Object.entries({
    position: "fixed", inset: "0", width: "100vw", height: "100vh", margin: "0", padding: "0",
    border: "0", background: "transparent", overflow: "visible", "pointer-events": "none",
    "z-index": "2147483647", display: "block", "max-width": "none", "max-height": "none",
    opacity: "1", visibility: "visible", transform: "none", filter: "none", contain: "strict",
  })) host.style.setProperty(name, value, "important");
  const root = host.attachShadow({ mode: "closed" });
  const sheet = new CSSStyleSheet();
  sheet.replaceSync(${"`"}
    :host { all: initial; }
    * { box-sizing: border-box; }
    .box {
      position: fixed; left: 0; top: 0; width: 0; height: 0; border-radius: 3px; opacity: 0;
      box-shadow: 0 0 0 2px var(--accent), 0 0 0 5px color-mix(in srgb, var(--accent) 22%, transparent);
      background: color-mix(in srgb, var(--accent) 12%, transparent); transition: opacity .12s ease;
    }
    .box.on { opacity: 1; }
    .box.taken { animation: taken .45s ease-out; }
    @keyframes taken { from { background: color-mix(in srgb, var(--accent) 40%, transparent); } }
    .pill {
      position: fixed; padding: 4px 9px 5px; border-radius: 999px;
      background: rgba(28, 26, 25, .92); color: #f4efe7; white-space: nowrap; overflow: hidden; text-overflow: ellipsis;
      font: 500 12px/1.3 -apple-system, BlinkMacSystemFont, "Segoe UI", Inter, Roboto, Arial, sans-serif;
      letter-spacing: .005em; box-shadow: 0 0 0 1px rgba(255, 255, 255, .1), 0 4px 14px rgba(0, 0, 0, .28);
    }
    .tag { left: 0; top: 0; max-width: min(320px, calc(100vw - 8px)); opacity: 0; transition: opacity .12s ease; }
    .tag.on { opacity: 1; }
    .size { margin-inline-start: 6px; color: rgba(244, 239, 231, .62); font-variant-numeric: tabular-nums; }
    .hint {
      left: 50%; top: 12px; transform: translateX(-50%); width: max-content; max-width: calc(100vw - 24px);
      white-space: normal; text-align: center; border-radius: 12px;
    }
    .hint.low { top: auto; bottom: 12px; }
    .hint.off { display: none; }
    @media (prefers-reduced-motion: reduce) { .box, .tag { transition: none; } .box.taken { animation: none; } }
  ${"`"});
  root.adoptedStyleSheets = [sheet];
  const element = (name) => { const node = doc.createElement("div"); node.className = name; return node; };
  const box = element("box"), tag = element("pill tag"), hint = element("pill hint");
  const what = doc.createElement("span"), size = doc.createElement("span");
  size.className = "size";
  tag.append(what, size);
  root.append(box, tag, hint);
  // While picking, frames give their input to this document, so a frame can be
  // outlined and picked as a whole, and the pointer says what a click does.
  const page = new CSSStyleSheet();
  page.replaceSync("iframe, frame, embed, object { pointer-events: none !important; } html, html * { cursor: crosshair !important; }");
  let on = false, current = null, frame = 0, x = -1, y = -1, exact = false, more = false, leave = 0;

  const squash = (text) => String(text ?? "").replace(/\s+/g, " ").trim();
  const cut = (text, max) => (text.length > max ? text.slice(0, max - 1) + "…" : text);
  const raise = () => {
    if (!host.isConnected) (doc.documentElement || doc.body)?.append(host);
    // Re-entering the top layer keeps the outline above dialogs the page opened since.
    if (host.showPopover) {
      try { if (host.matches(":popover-open")) host.hidePopover(); host.showPopover(); } catch {}
    }
  };
  /** The innermost element at a point, inside open shadow roots too. */
  const top = (px, py) => {
    let node = doc.elementFromPoint(px, py);
    while (node?.shadowRoot) {
      const inner = node.shadowRoot.elementFromPoint(px, py);
      if (!inner || inner === node) break;
      node = inner;
    }
    return node;
  };
  const target = (px, py, alt) => {
    let node = top(px, py);
    if (!node || node === host || node === doc.documentElement || node === doc.body) return null;
    if (alt) return node;
    if (node instanceof SVGElement && node.ownerSVGElement) node = node.ownerSVGElement;
    return node.closest(CONTROL) ?? node;
  };
  /** "button#save.primary": the tag, its id and first two classes. */
  const badge = (node) => {
    const classes = [...node.classList].filter((item) => /^[\w-]+$/.test(item)).slice(0, 2);
    return cut(node.localName + (node.id ? "#" + node.id : "") + classes.map((item) => "." + item).join(""), 80);
  };
  const named = (node) => {
    const label = squash(node.getAttribute("aria-label"));
    if (label) return label;
    const ids = node.getAttribute("aria-labelledby");
    if (ids) {
      const scope = node.getRootNode();
      const text = squash(ids.split(/\s+/).map((id) => (scope.getElementById?.(id) ?? doc.getElementById(id))?.textContent ?? "").join(" "));
      if (text) return text;
    }
    for (const name of ["alt", "title", "placeholder"]) {
      const value = squash(node.getAttribute(name));
      if (value) return value;
    }
    if (node instanceof HTMLInputElement && ["button", "submit", "reset"].includes(node.type)) return squash(node.value);
    return node.matches(NAMED_BY_TEXT) ? squash(node.innerText ?? node.textContent) : "";
  };
  /** A CSS path from the nearest unique id; " >>> " steps into a shadow root. */
  const selector = (node) => {
    const segments = [];
    for (let start = node; start; ) {
      const scope = start.getRootNode();
      const path = [];
      for (let step = start; step; step = step.parentElement) {
        if (step.id && scope.querySelectorAll?.("#" + CSS.escape(step.id)).length === 1) {
          path.unshift("#" + CSS.escape(step.id));
          break;
        }
        if (step === doc.body || step === doc.documentElement) {
          path.unshift(step.localName);
          break;
        }
        const same = [...(step.parentNode?.children ?? [])].filter((item) => item.localName === step.localName);
        path.unshift(CSS.escape(step.localName) + (same.length > 1 ? ":nth-of-type(" + (same.indexOf(step) + 1) + ")" : ""));
      }
      segments.unshift(path.join(" > "));
      start = scope instanceof ShadowRoot ? scope.host : null;
    }
    const value = segments.join(" >>> ");
    // A cut selector is wrong; none is better.
    return value.length > 300 ? "" : value;
  };
  /** Outer HTML in one line, long attribute values cut, at most MAX_HTML. */
  const html = (node) => {
    const value = node.outerHTML.replace(/\s+/g, " ").replace(/(=")([^"]{120})[^"]*"/g, '$1$2…"');
    if (value.length <= MAX_HTML) return value;
    const close = "</" + node.localName + ">";
    return value.slice(0, MAX_HTML - close.length - 1) + "…" + (value.endsWith(close) ? close : "");
  };
  const details = (node) => {
    const rect = node.getBoundingClientRect();
    const name = cut(named(node), 120);
    const text = cut(squash(node.innerText ?? node.textContent), 300);
    return {
      url: cut(location.href, 2048),
      tag: badge(node),
      name,
      text: text === name ? "" : text,
      html: html(node),
      selector: selector(node),
      width: Math.round(rect.width),
      height: Math.round(rect.height),
    };
  };
  const clear = () => {
    current = null;
    box.classList.remove("on");
    tag.classList.remove("on");
  };
  const show = (node) => {
    const rect = node.getBoundingClientRect();
    box.style.transform = "translate(" + rect.left + "px," + rect.top + "px)";
    box.style.width = rect.width + "px";
    box.style.height = rect.height + "px";
    box.classList.add("on");
    if (current !== node) {
      current = node;
      what.textContent = badge(node);
      raise();
    }
    size.textContent = Math.round(rect.width) + "×" + Math.round(rect.height);
    // The label sits above the outline, below it near the top, inside it when neither fits.
    const label = tag.getBoundingClientRect();
    const left = Math.min(Math.max(rect.left, 4), Math.max(innerWidth - label.width - 4, 4));
    const above = rect.top - label.height - 6, below = rect.bottom + 6;
    const at = above >= 4 ? above : below + label.height <= innerHeight - 4 ? below : Math.max(rect.top + 4, 4);
    tag.style.transform = "translate(" + left + "px," + at + "px)";
    tag.classList.add("on");
  };
  const update = () => {
    frame = 0;
    if (!on || x < 0) return;
    hint.classList.toggle("low", y < 64);
    const node = target(x, y, exact);
    if (node) show(node);
    else clear();
  };
  const schedule = () => { if (on && !frame) frame = requestAnimationFrame(update); };
  const move = (event) => {
    if (!event.isTrusted) return;
    x = event.clientX; y = event.clientY; exact = event.altKey; more = event.shiftKey;
    schedule();
  };
  const out = (event) => { if (!event.relatedTarget) { x = -1; clear(); } };
  const alt = (event) => {
    more = event.shiftKey;
    if (event.key === "Alt") { exact = event.type === "keydown"; schedule(); }
  };
  const listen = (add) => {
    const method = add ? addEventListener : removeEventListener;
    method("mousemove", move, { capture: true, passive: true });
    method("mouseout", out, true);
    method("keydown", alt, true);
    method("keyup", alt, true);
    method("scroll", schedule, { capture: true, passive: true });
    method("resize", schedule);
  };
  const adopt = (add) => {
    try {
      const others = doc.adoptedStyleSheets.filter((item) => item !== page);
      doc.adoptedStyleSheets = add ? [...others, page] : others;
    } catch {}
  };
  return {
    start(input) {
      clearTimeout(leave);
      host.style.setProperty("--accent", input.accent || "#d9874b");
      hint.textContent = input.hint || "";
      hint.classList.toggle("off", !input.hint);
      box.classList.remove("taken");
      clear();
      raise();
      if (!on) { on = true; listen(true); adopt(true); }
      schedule();
      return true;
    },
    /**
     * The element a press at the point picks, outlined as taken, and whether
     * Shift was down as the pointer last moved; none over nothing.
     */
    take(px, py, alt) {
      if (!on) return null;
      const node = target(px, py, alt || exact) ?? current;
      if (!node?.isConnected) return null;
      show(node);
      box.classList.remove("taken");
      void box.offsetWidth;
      box.classList.add("taken");
      return { element: details(node), more };
    },
    /** A pick stays outlined a moment, so it reads as taken. */
    stop(picked) {
      if (!on) return;
      on = false;
      listen(false);
      adopt(false);
      cancelAnimationFrame(frame);
      frame = 0;
      hint.classList.add("off");
      tag.classList.remove("on");
      leave = setTimeout(() => { clear(); host.remove(); }, picked ? 450 : 0);
    },
    /** Screenshots hide it, so the agent never sees it. */
    hide(value) {
      host.style.setProperty("visibility", value ? "hidden" : "visible", "important");
    },
  };
})();`;

type Callbacks = {
  picked: (element: PickedElement, more: boolean) => void;
  ended: (reason: PickEnd) => void;
};

/** Which modifier keys are down, as the last key event that reached a view said. */
export function createModifiers() {
  let held = { shift: false, alt: false };
  return {
    input(input: Pick<Electron.Input, "type" | "shift" | "alt">) {
      if (input.type === "keyDown" || input.type === "rawKeyDown" || input.type === "keyUp")
        held = { shift: input.shift, alt: input.alt };
    },
    get: () => held,
  };
}

/**
 * Picking in one tab's page. `zoom` is the page's zoom factor (presses come in
 * view pixels, the page measures in CSS pixels); `agent` says an agent's own
 * input is being dispatched to the page right now, which passes untouched.
 * Presses do not say which modifiers are down, so Shift and Option come from
 * the keys the page and its window (`window`) last heard, and from the
 * pointer's last move.
 */
export function createPicker(
  contents: Pick<WebContents, "executeJavaScriptInIsolatedWorld" | "isDestroyed">,
  options: {
    zoom: () => number;
    agent: () => boolean;
    window?: () => { shift: boolean; alt: boolean };
  },
) {
  const keys = createModifiers();
  const held = () => {
    const page = keys.get(), window = options.window?.() ?? { shift: false, alt: false };
    return { shift: page.shift || window.shift, alt: page.alt || window.alt };
  };
  let session: Callbacks | undefined;
  // A press whose release the page must not see either.
  let pressed = false;
  let swallow = { until: 0, up: false };
  // Until when the outline may still be on screen, for screenshots.
  let lingers = 0;
  const run = <T>(code: string) =>
    contents.isDestroyed()
      ? Promise.reject(new Error("Browser tab was closed"))
      : (contents.executeJavaScriptInIsolatedWorld(PICK_WORLD, [
          { code: `${pickerScript};${code}` },
        ]) as Promise<T>);
  const call = (method: string, ...args: unknown[]) =>
    run(
      `globalThis.__haarnessPicker.${method}(${args.map((value) => JSON.stringify(value ?? null)).join(",")})`,
    );
  function end(reason: PickEnd) {
    const current = session;
    if (!current) return;
    session = undefined;
    if (reason === "picked")
      swallow = { until: Date.now() + SWALLOW_MS, up: pressed };
    lingers = Date.now() + 1_000;
    void call("stop", reason === "picked").catch(() => {});
    current.ended(reason);
  }
  async function take(input: Electron.MouseInputEvent) {
    const current = session;
    const zoom = options.zoom() || 1;
    const keys = held();
    const taken = (await call("take", input.x / zoom, input.y / zoom, keys.alt).catch(
      () => null,
    )) as { element: PickedElement; more: boolean } | null;
    if (session !== current || !current || !taken) return;
    const more = keys.shift || taken.more;
    current.picked(taken.element, more);
    if (!more) end("picked");
  }
  return {
    get active() {
      return !!session;
    },
    async start(request: PickRequest, callbacks: Callbacks) {
      end("stopped");
      const current = (session = callbacks);
      pressed = false;
      try {
        await call("start", request);
      } catch (error) {
        if (session === current) session = undefined;
        throw new Error("This page cannot be picked from. Reload it and try again.", {
          cause: error,
        });
      }
    },
    end,
    /** A press in the page: true when the picker kept it from the page. */
    mouse(event: Electron.Event, input: Electron.MouseInputEvent) {
      const press =
        input.type === "mouseDown" || input.type === "mouseUp" || input.type === "contextMenu";
      if (!press || options.agent()) return false;
      if (!session) {
        // The release of the press that picked, and the rest of a double click.
        const swallowed =
          (input.type === "mouseUp" && swallow.up) || Date.now() < swallow.until;
        if (input.type === "mouseUp") swallow.up = false;
        if (swallowed) event.preventDefault();
        return swallowed;
      }
      event.preventDefault();
      if (input.type === "mouseDown") pressed = true;
      if (input.type === "mouseUp") pressed = false;
      if (input.type === "mouseDown" && input.button === "left") void take(input);
      return true;
    },
    /** A key in the page: Escape stops picking. */
    key(event: Electron.Event, input: Electron.Input) {
      if (options.agent()) return false;
      keys.input(input);
      if (!session || input.type !== "keyDown" || input.key !== "Escape") return false;
      event.preventDefault();
      end("cancelled");
      return true;
    },
    /** Runs `capture` with the outline hidden while it may be on screen. */
    async hidden<T>(capture: () => Promise<T>): Promise<T> {
      if (!session && Date.now() > lingers) return capture();
      await call("hide", true).catch(() => {});
      try {
        return await capture();
      } finally {
        await call("hide", false).catch(() => {});
      }
    },
  };
}

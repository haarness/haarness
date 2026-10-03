/**
 * The agent's visible cursor: while Haarness controls a tab, a pointer glides to
 * each target, a ripple marks each click, a short label says what is happening
 * ("Clicking “Sign in”", "Typing", "Scrolling") and a soft edge glow shows the
 * tab is being driven — the presentation Claude in Chrome and Codex use.
 *
 * It is drawn inside the page so it stays exactly on the content it points at,
 * but it is built to change nothing about the page:
 *   - it runs in an isolated JavaScript world, so page scripts cannot see,
 *     call or break it, and it survives the page replacing its own globals;
 *   - it lives in a closed shadow root on a top-layer popover, so page CSS
 *     cannot style it and modals cannot cover it;
 *   - it ignores the pointer, is inert and aria-hidden, so real input lands on
 *     the page and accessibility snapshots never include it;
 *   - styles go through constructable stylesheets and the CSSOM, which strict
 *     page CSPs allow;
 *   - screenshots hide it first, so the agent never sees its own cursor.
 * A new document starts a new world, so every call re-installs on demand.
 */
import type { WebContents } from "electron";

/** Any stable id outside Electron's own worlds; one per page document. */
const WORLD = 1_779;

/** Installs the overlay in the current document once and returns its API. */
const INSTALL = String.raw`globalThis.__haarnessCursor ??= (() => {
  const doc = document;
  const host = doc.createElement("haarness-agent-cursor");
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
    .edge {
      position: fixed; inset: 0; border-radius: 0; opacity: 0;
      box-shadow: inset 0 0 0 2px rgba(219, 145, 92, .85), inset 0 0 28px 2px rgba(219, 145, 92, .28);
      transition: opacity .35s ease;
    }
    .edge.on { opacity: 1; animation: breathe 2.4s ease-in-out infinite; }
    @keyframes breathe { 50% { box-shadow: inset 0 0 0 2px rgba(219, 145, 92, .6), inset 0 0 40px 4px rgba(219, 145, 92, .2); } }
    .cursor {
      position: fixed; left: 0; top: 0; width: 0; height: 0; opacity: 0;
      transition: opacity .12s ease; will-change: transform;
    }
    .cursor.on { opacity: 1; }
    .pointer {
      position: absolute; left: -3px; top: -2px; width: 24px; height: 24px;
      filter: drop-shadow(0 1px 1.5px rgba(0, 0, 0, .35)) drop-shadow(0 3px 8px rgba(0, 0, 0, .18));
      transform-origin: 3px 2px; transition: transform .12s ease;
    }
    .pointer.press { transform: scale(.82); }
    .label {
      position: absolute; left: 18px; top: 22px; max-width: 260px;
      padding: 4px 9px 5px; border-radius: 999px;
      background: rgba(28, 26, 25, .92); color: #f4efe7;
      font: 500 12px/1.3 -apple-system, BlinkMacSystemFont, "Segoe UI", Inter, Roboto, Arial, sans-serif;
      letter-spacing: .005em; white-space: nowrap; overflow: hidden; text-overflow: ellipsis;
      box-shadow: 0 0 0 1px rgba(255, 255, 255, .1), 0 4px 14px rgba(0, 0, 0, .28);
      opacity: 0; transform: translateY(-2px); transition: opacity .18s ease, transform .18s ease;
    }
    .label.on { opacity: 1; transform: none; }
    .cursor.flip-x .label { left: auto; right: 6px; }
    .cursor.flip-y .label { top: auto; bottom: 8px; }
    .label .dots::after { content: "…"; animation: dots 1.2s steps(4) infinite; display: inline-block; width: 1.1em; overflow: hidden; vertical-align: bottom; }
    @keyframes dots { from { width: 0; } }
    .ripple {
      position: fixed; width: 36px; height: 36px; margin: -18px 0 0 -18px; border-radius: 50%;
      border: 2px solid rgba(219, 145, 92, .95); background: rgba(219, 145, 92, .18);
      animation: ripple .55s cubic-bezier(.2, .6, .3, 1) forwards;
    }
    @keyframes ripple { from { transform: scale(.25); opacity: 1; } to { transform: scale(1.35); opacity: 0; } }
    @media (prefers-reduced-motion: reduce) {
      .edge.on { animation: none; }
      .ripple { animation-duration: .01s; }
      .label .dots::after { animation: none; width: auto; }
    }
  ${"`"});
  root.adoptedStyleSheets = [sheet];
  const edge = doc.createElement("div");
  edge.className = "edge";
  const cursor = doc.createElement("div");
  cursor.className = "cursor";
  // An arrow drawn for light and dark pages alike: copper fill, white keyline.
  const svg = doc.createElementNS("http://www.w3.org/2000/svg", "svg");
  svg.setAttribute("viewBox", "0 0 24 24");
  svg.setAttribute("class", "pointer");
  const path = doc.createElementNS("http://www.w3.org/2000/svg", "path");
  path.setAttribute("d", "M4.2 2.6 19.4 11.3c.9.5.8 1.8-.2 2.1l-6.1 1.9-3 5.6c-.5.9-1.8.8-2.1-.2L3 4.1c-.3-1 .6-1.9 1.2-1.5Z");
  path.setAttribute("fill", "#d9874b");
  path.setAttribute("stroke", "#ffffff");
  path.setAttribute("stroke-width", "1.6");
  path.setAttribute("stroke-linejoin", "round");
  svg.append(path);
  const label = doc.createElement("div");
  label.className = "label";
  cursor.append(svg, label);
  root.append(edge, cursor);

  const reduced = () => matchMedia("(prefers-reduced-motion: reduce)").matches;
  let at = null, hidden = false, idle = 0, glow = 0, motion = null;
  const raise = () => {
    if (!host.isConnected) (doc.documentElement || doc.body)?.append(host);
    // Re-entering the top layer keeps the cursor above dialogs the page opened since.
    if (host.showPopover) {
      try { if (host.matches(":popover-open")) host.hidePopover(); host.showPopover(); } catch {}
    }
  };
  const awake = () => {
    raise();
    clearTimeout(idle); clearTimeout(glow);
    edge.classList.add("on");
    if (!hidden) cursor.classList.add("on");
    // The pointer rests where it last acted, then fades once the agent goes quiet.
    glow = setTimeout(() => edge.classList.remove("on"), 2600);
    idle = setTimeout(() => { cursor.classList.remove("on"); label.classList.remove("on"); }, 7000);
  };
  const say = (text, busy) => {
    label.textContent = "";
    if (!text) { label.classList.remove("on"); return; }
    label.append(text);
    if (busy) { const dots = doc.createElement("span"); dots.className = "dots"; label.append(dots); }
    label.classList.add("on");
  };
  const place = (x, y) => { cursor.style.transform = "translate(" + x + "px," + y + "px)"; };
  const move = (x, y, text, busy, last) => {
    awake();
    // Keep the label inside the viewport near the right and bottom edges.
    cursor.classList.toggle("flip-x", x > innerWidth - 290);
    cursor.classList.toggle("flip-y", y > innerHeight - 56);
    say(text, busy);
    motion?.cancel();
    // A new document starts where the previous one left the pointer.
    const from = at ?? last ?? { x: x - 48, y: y + 64 };
    at = { x, y };
    const distance = Math.hypot(x - from.x, y - from.y);
    if (reduced() || distance < 2) { place(x, y); return Promise.resolve(); }
    // A gentle arc, eased in and out: it reads as a hand moving, not a teleport.
    // Quick enough to keep a long task moving, slow enough to follow.
    const duration = Math.min(420, Math.max(180, 140 + distance * 0.32));
    const bend = Math.min(60, distance * 0.12);
    const mx = (from.x + x) / 2 - ((y - from.y) / (distance || 1)) * bend;
    const my = (from.y + y) / 2 + ((x - from.x) / (distance || 1)) * bend;
    const frames = [];
    for (let i = 0; i <= 16; i++) {
      const t = i / 16, e = t < .5 ? 4 * t * t * t : 1 - Math.pow(-2 * t + 2, 3) / 2;
      const px = (1 - e) * (1 - e) * from.x + 2 * (1 - e) * e * mx + e * e * x;
      const py = (1 - e) * (1 - e) * from.y + 2 * (1 - e) * e * my + e * e * y;
      frames.push({ transform: "translate(" + px + "px," + py + "px)" });
    }
    place(x, y);
    motion = cursor.animate(frames, { duration, easing: "linear" });
    return motion.finished.then(() => undefined, () => undefined);
  };
  const click = (x, y) => {
    awake();
    svg.classList.add("press");
    setTimeout(() => svg.classList.remove("press"), 140);
    const ring = doc.createElement("div");
    ring.className = "ripple";
    ring.style.left = x + "px";
    ring.style.top = y + "px";
    root.append(ring);
    setTimeout(() => ring.remove(), 700);
  };
  const note = (text, busy) => { awake(); say(text, busy); };
  const hide = (value) => {
    hidden = value;
    host.style.setProperty("visibility", value ? "hidden" : "visible", "important");
    if (!value) raise();
  };
  return { move, click, note, hide };
})();`;

export interface AgentCursor {
  /** Glides to a point and resolves when it arrives, so input lands as shown. */
  move(point: { x: number; y: number }, label: string, busy?: boolean): Promise<void>;
  /** Marks a click at the point the cursor just reached. */
  click(point: { x: number; y: number }): void;
  /** Changes the label without moving, as for a key press. */
  note(label: string, busy?: boolean): void;
  /** Runs `capture` with the overlay hidden, so screenshots never show it. */
  hidden<T>(capture: () => Promise<T>): Promise<T>;
}

/** English keys for each action, alone and with its target's accessible name. */
const LABELS = {
  click: ["Clicking", "Clicking “{name}”"],
  point: ["Pointing", "Pointing at “{name}”"],
  drag: ["Dragging", "Dragging “{name}”"],
  drop: ["Dropping", "Dropping on “{name}”"],
  type: ["Typing", "Typing in “{name}”"],
  choose: ["Choosing", "Choosing in “{name}”"],
  check: ["Checking", "Checking “{name}”"],
  uncheck: ["Unchecking", "Unchecking “{name}”"],
} as const;
export type CursorAction = keyof typeof LABELS;
export const cursorLabelKeys = [
  ...Object.values(LABELS).flat(),
  "Pressing {key}",
  "Scrolling",
  "Scrolling up",
  "Scrolling down",
];

/** At most 32 characters, cut at a word where one is near. */
function brief(text: string) {
  if (text.length <= 32) return text;
  const cut = text.slice(0, 31);
  const space = cut.lastIndexOf(" ");
  return (space > 16 ? cut.slice(0, space) : cut).replace(/[\s,.;:–-]+$/, "") + "…";
}

/** A short label for an action, naming its target when it has a name. */
export function cursorLabel(
  action: CursorAction,
  name?: string,
  translate: (key: string) => string = (key) => key,
): string {
  const text = name?.replace(/\s+/g, " ").trim();
  const [alone, named] = LABELS[action];
  if (!text) return translate(alone);
  return translate(named).replace("{name}", brief(text));
}

export function createAgentCursor(
  contents: WebContents,
  enabled: () => boolean,
): AgentCursor {
  // Input coordinates are in view pixels; the overlay draws in CSS pixels.
  let last: { x: number; y: number } | undefined;
  const css = (point: { x: number; y: number }) => {
    const zoom = contents.isDestroyed() ? 1 : contents.getZoomFactor() || 1;
    return { x: point.x / zoom, y: point.y / zoom };
  };
  const run = async (expression: string) => {
    if (contents.isDestroyed() || !enabled()) return;
    try {
      await contents.executeJavaScriptInIsolatedWorld(WORLD, [
        { code: `${INSTALL};${expression}` },
      ]);
    } catch {
      // A page mid-navigation or a closed tab: the cursor is decoration and
      // must never fail the action it accompanies.
    }
  };
  const call = (method: string, ...args: unknown[]) =>
    run(`globalThis.__haarnessCursor.${method}(${args.map((value) => JSON.stringify(value ?? null)).join(",")})`);
  return {
    async move(point, label, busy = false) {
      const at = css(point);
      await call("move", at.x, at.y, label, busy, last ?? null);
      last = at;
    },
    click(point) {
      const at = css(point);
      void call("click", at.x, at.y);
    },
    note: (label, busy = false) => void call("note", label, busy),
    async hidden(capture) {
      await call("hide", true);
      try {
        return await capture();
      } finally {
        await call("hide", false);
      }
    },
  };
}

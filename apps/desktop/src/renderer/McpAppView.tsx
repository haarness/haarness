import { t, language, errorLine } from "./i18n";
import { useApp } from "./state";
import { Icon } from "./ui";
import { MenuButton } from "./MenuButton";
import {
  Show,
  createEffect,
  createSignal,
  on,
  onCleanup,
  onMount,
} from "solid-js";
import {
  DEFAULT_HEIGHT,
  HOVER,
  INPUT,
  LEFT,
  MAX_HEIGHT,
  PROTOCOL_VERSION,
  WHEEL,
  createBridge,
  document as pageDocument,
  granted,
  linkScheme,
  recall,
  remember,
  resource,
  themeTokens,
  variables,
  type Meta,
  type Payload,
} from "./mcp-app";

/** A page's HTML, read once a minute at most per server and page; the most recent few are kept. */
const pages = new Map<string, { at: number; value: Promise<{ html: string; meta: Meta }> }>();
const PAGE_LIMIT = 32;
/** The last height each page reported, so a row that remounts keeps its size; the most recent kept. */
const heights = new Map<string, number>();
const HEIGHT_LIMIT = 512;
/** Every token the page's theme variables come from, read as written to tell whether the theme moved. */
const THEME_TOKENS = themeTokens();

function readPage(connection: string, server: string, uri: string) {
  const key = [connection, server, uri].join("\u0000");
  const cached = recall(pages, key);
  if (cached && Date.now() - cached.at < 60_000) return cached.value;
  const value = window.haarness
    .api<unknown>(connection, "readMcpResource", [server, { uri }])
    .then((result) => {
      const page = resource(result, uri);
      if (!page) throw new Error(t("The server sent no page for this app."));
      return page;
    });
  remember(pages, key, { at: Date.now(), value }, PAGE_LIMIT);
  value.catch(() => pages.get(key)?.value === value && pages.delete(key));
  return value;
}

/** Colour tokens resolved to plain colours: several are `color-mix()` of other tokens. */
function resolveColor(probe: HTMLElement, token: string) {
  probe.style.color = `var(${token})`;
  return getComputedStyle(probe).color;
}

/**
 * An MCP App page under its tool's row: the server's `ui://` page in a sandboxed frame with the
 * policy its server declared, answered through the bridge in mcp-app.ts.
 */
export function McpAppView(props: {
  payload: Payload;
  arguments: unknown;
  /** Identifies the call, for the page's remembered height. */
  callKey: string;
  /** Turns this server's interactive apps off; absent where that cannot be changed (a remote host). */
  turnOff?: () => void;
}) {
  const app = useApp();
  const connection = app.state.connection?.id ?? "";
  const thread = app.state.selected || undefined;
  const id = crypto.randomUUID();
  const label = () => props.payload.tool.title || props.payload.tool.name || props.payload.server;
  const [page, setPage] = createSignal<{ html: string; meta: Meta }>();
  const [failure, setFailure] = createSignal("");
  const [hostReady, setHostReady] = createSignal(false);
  const [loaded, setLoaded] = createSignal(false);
  const [left, setLeft] = createSignal(false);
  const [height, setHeight] = createSignal(recall(heights, props.callKey) ?? 0);
  const [request, setRequest] = createSignal<{ url: string; answer: (open: boolean) => void }>();
  const [message, setMessage] = createSignal<{ text: string; answer: (insert: boolean) => void }>();
  /** The scheme of the last link Haarness refused after a press in the page ("" when it has none). */
  const [refused, setRefused] = createSignal<string>();
  const [render, setRender] = createSignal(0);
  /** The pointer is over the page, which the window cannot tell past the frame. */
  const [pointed, setPointed] = createSignal(false);
  let root!: HTMLDivElement;
  let frame!: HTMLIFrameElement;
  let probe!: HTMLSpanElement;
  let token = "";
  let initialized = false;
  /** The theme the page last heard about. */
  let lastTheme = "";
  /** The person pressed in the page since the last link or message went through without asking. */
  let pressed = false;

  const load = () => {
    setFailure("");
    readPage(connection, props.payload.server, props.payload.uri).then(
      (value) => setPage(value),
      (error: unknown) => setFailure(errorLine(error)),
    );
  };
  const width = () => Math.round(root?.clientWidth ?? 0);
  const theme = () => (globalThis.document.documentElement.dataset.theme === "light" ? "light" : "dark");
  const styles = () => {
    const root = getComputedStyle(globalThis.document.documentElement);
    return {
      variables: variables(
        (token) => root.getPropertyValue(token),
        (token) => resolveColor(probe, token),
      ),
    };
  };
  const context = () => ({
    theme: theme(),
    styles: styles(),
    displayMode: "inline",
    availableDisplayModes: ["inline"],
    containerDimensions: { width: width(), maxHeight: MAX_HEIGHT },
    locale: language(),
    timeZone: Intl.DateTimeFormat().resolvedOptions().timeZone,
    userAgent: "Haarness Desktop",
    platform: "desktop",
    deviceCapabilities: { touch: false, hover: true },
    toolInfo: { tool: { inputSchema: { type: "object" }, ...props.payload.tool } },
  });
  // Results and arguments come from the app's store, whose proxies cannot cross postMessage:
  // the page gets plain JSON copies.
  const send = (message: Record<string, unknown>) =>
    frame?.contentWindow?.postMessage({ mcpApp: id, type: "send", message: JSON.parse(JSON.stringify(message)) }, "*");
  const bridge = createBridge(
    {
      initialize: () => ({
        protocolVersion: PROTOCOL_VERSION,
        hostInfo: { name: "Haarness Desktop", version: "1" },
        hostCapabilities: {
          openLinks: {},
          serverResources: {},
          message: { text: {} },
          logging: {},
          sandbox: {
            csp: granted(page()?.meta),
            permissions: page()?.meta.permissions?.clipboardWrite ? { clipboardWrite: {} } : {},
          },
        },
        hostContext: context(),
      }),
      initialized: () => {
        if (initialized) return;
        initialized = true;
        lastTheme = JSON.stringify({ theme: theme(), styles: styles() });
        bridge.notify("ui/notifications/tool-input", {
          arguments:
            props.arguments && typeof props.arguments === "object" && !Array.isArray(props.arguments)
              ? props.arguments
              : {},
        });
        if (props.payload.result) bridge.notify("ui/notifications/tool-result", props.payload.result);
        else bridge.notify("ui/notifications/tool-cancelled", { reason: "The result is too large to show in the app." });
      },
      resize: ({ height }) => {
        if (height === undefined) return;
        const next = Math.max(48, Math.min(MAX_HEIGHT, Math.ceil(height)));
        remember(heights, props.callKey, next, HEIGHT_LIMIT);
        setHeight(next);
      },
      // The page hears why; the person, who may have clicked it, sees it under the page.
      // Only a press in the page earns the notice, once per press, so a page
      // cannot repeat it by itself; and it names only the link's scheme,
      // never the page's own text.
      refusedLink: (url, active) => {
        if (take(active)) setRefused(linkScheme(url) ?? "");
      },
      openLink: (url, active) => {
        if (take(active)) return open(url);
        return new Promise<boolean>((resolve) => {
          request()?.answer(false);
          setRequest({
            url,
            answer: (accepted) => {
              setRequest(undefined);
              resolve(accepted ? open(url) : false);
            },
          });
        });
      },
      // A message goes into the message box, and the keyboard with it, only
      // when the person just acted in the page or picks "Insert app message":
      // a page writing on its own must not change a draft or take the focus.
      compose: (text, active) => {
        if (take(active)) return insert(text);
        return new Promise<boolean>((resolve) => {
          message()?.answer(false);
          setMessage({
            text,
            answer: (accepted) => {
              setMessage(undefined);
              resolve(accepted ? insert(text) : false);
            },
          });
        });
      },
      readResource: (uri) =>
        window.haarness.api(connection, "readMcpResource", [props.payload.server, { uri }]),
      log: (params) => console.debug("MCP App", props.payload.server, params),
    },
    send,
  );
  /**
   * One action for each click or key press in the page: the page's document
   * reported the person's press, the frame's host page saw the activation
   * when the request arrived, the page has the focus, and no other link or
   * message has used that press.
   */
  function take(active: boolean) {
    if (!active || !pressed || globalThis.document.activeElement !== frame) return false;
    pressed = false;
    return true;
  }
  function insert(text: string) {
    const detail = { text, accepted: false };
    root.dispatchEvent(new CustomEvent("mcp-app-message", { bubbles: true, detail }));
    if (detail.accepted) app.notify(t("The app's message is in the message box. Send it when you are ready."));
    else app.notify(t("This chat cannot take a message from the app."), "blocked");
    return detail.accepted;
  }
  async function open(url: string) {
    try {
      await window.haarness.openLink(connection || undefined, thread, url);
      return true;
    } catch {
      app.notify(t("This link could not be opened."), "blocked");
      return false;
    }
  }
  // The frame's host page is ready and the page is read: put the page in.
  createEffect(
    on([hostReady, page, render] as const, ([ready, value]) => {
      if (!ready || !value) return;
      token = crypto.randomUUID();
      pressed = false;
      initialized = false;
      setLoaded(false);
      setLeft(false);
      frame.contentWindow?.postMessage(
        {
          mcpApp: id,
          type: "render",
          title: t("{name} app", { name: label() }),
          clipboard: !!value.meta.permissions?.clipboardWrite,
          html: pageDocument(value.html, value.meta, token),
        },
        "*",
      );
    }),
  );
  // The page follows the theme and language as they change.
  createEffect(
    on(language, (locale) => initialized && bridge.notify("ui/notifications/host-context-changed", { locale }), {
      defer: true,
    }),
  );
  onMount(() => {
    load();
    const listen = (event: MessageEvent) => {
      if (event.source !== frame.contentWindow) return;
      const data = event.data as Record<string, unknown> | undefined;
      if (!data || typeof data !== "object") return;
      if (data.type === "mcp-app-host-ready") return void setHostReady(true);
      if (data.mcpApp !== id) return;
      if (data.type === "loaded") {
        setLoaded(true);
        // A page that never says how tall it is still shows.
        if (!height()) setHeight(DEFAULT_HEIGHT);
        return;
      }
      if (data.type === "navigation") return void setLeft(true);
      if (data.type !== "receive") return;
      const message = data.message as Record<string, unknown> | undefined;
      if (message && Array.isArray(message[WHEEL])) return wheel(message[WHEEL] as number[]);
      if (message && HOVER in message) {
        const [value, from] = Array.isArray(message[HOVER]) ? (message[HOVER] as unknown[]) : [];
        if (from === token) setPointed(value === 1);
        return;
      }
      if (message && INPUT in message) {
        if (message[INPUT] === token) pressed = true;
        return;
      }
      if (message && LEFT in message) {
        if (message[LEFT] === token) setLeft(true);
        return;
      }
      void bridge.receive(message, data.active === true);
    };
    window.addEventListener("message", listen);
    // The root's style also carries layout variables, which change often:
    // changes are taken once a frame, and the theme's own tokens, read as
    // written, tell whether it moved before every colour is resolved again.
    let themeFrame = 0;
    let lastSignature = "";
    const themeObserver = new MutationObserver(() => {
      if (!initialized || themeFrame) return;
      themeFrame = requestAnimationFrame(() => {
        themeFrame = 0;
        const root = getComputedStyle(globalThis.document.documentElement);
        const signature = [theme(), ...THEME_TOKENS.map((token) => root.getPropertyValue(token))].join("\n");
        if (signature === lastSignature) return;
        lastSignature = signature;
        const next = { theme: theme(), styles: styles() };
        const text = JSON.stringify(next);
        if (text === lastTheme) return;
        lastTheme = text;
        bridge.notify("ui/notifications/host-context-changed", next);
      });
    });
    themeObserver.observe(globalThis.document.documentElement, {
      attributes: true,
      attributeFilter: ["style", "data-theme", "data-palette"],
    });
    let lastWidth = width();
    const resize = new ResizeObserver(() => {
      if (!initialized || width() === lastWidth) return;
      lastWidth = width();
      bridge.notify("ui/notifications/host-context-changed", {
        containerDimensions: { width: lastWidth, maxHeight: MAX_HEIGHT },
      });
    });
    resize.observe(root);
    onCleanup(() => {
      window.removeEventListener("message", listen);
      themeObserver.disconnect();
      cancelAnimationFrame(themeFrame);
      resize.disconnect();
      request()?.answer(false);
      message()?.answer(false);
      if (initialized) void bridge.request("ui/resource-teardown");
    });
  });
  /**
   * Wheel input over the page never reaches the conversation, so the page's document reports it:
   * the event goes to the frame element (the conversation stops following new output when scrolled
   * up) and the conversation scrolls as it would have.
   */
  function wheel([deltaX, deltaY, mode]: number[]) {
    if (![deltaX, deltaY, mode].every(Number.isFinite)) return;
    frame.dispatchEvent(new WheelEvent("wheel", { deltaX, deltaY, deltaMode: mode, bubbles: true }));
    let scroller = root.parentElement;
    while (scroller && !(scroller.scrollHeight > scroller.clientHeight && /auto|scroll/.test(getComputedStyle(scroller).overflowY)))
      scroller = scroller.parentElement;
    if (!scroller) return;
    const unit = mode === WheelEvent.DOM_DELTA_LINE ? 16 : mode === WheelEvent.DOM_DELTA_PAGE ? scroller.clientHeight : 1;
    scroller.scrollBy({ top: deltaY * unit, left: 0 });
  }
  return (
    <div
      class="mcp-app"
      ref={root}
      data-mcp-app={props.payload.uri}
      data-state={failure() ? "failed" : left() ? "left" : loaded() ? "ready" : "loading"}
      classList={{ bordered: page()?.meta.prefersBorder !== false, pointed: pointed() }}
    >
      <span ref={probe} class="mcp-app-probe" aria-hidden="true" />
      <Show when={props.turnOff}>
        {(turnOff) => (
          <div class="mcp-app-menu">
            <MenuButton
              icon="more"
              label={t("{name} app options", { name: label() })}
              items={[
                {
                  label: t("Turn off interactive apps for {server}", { server: props.payload.server }),
                  run: turnOff(),
                },
              ]}
            />
          </div>
        )}
      </Show>
      <Show when={!failure() && !left()}>
        <div class="mcp-app-frame" style={{ height: `${height() || DEFAULT_HEIGHT}px` }}>
          <iframe
            ref={frame}
            title={t("{name} app", { name: label() })}
            src="./mcp-app.html"
            sandbox="allow-scripts allow-forms"
            allow="clipboard-write"
            referrerpolicy="no-referrer"
            onLoad={() => setHostReady(true)}
          />
          <Show when={!loaded()}>
            <div class="mcp-app-loading" role="status">
              {t("Loading the app…")}
            </div>
          </Show>
        </div>
      </Show>
      <Show when={failure() || left()}>
        <div class="notice-inline mcp-app-notice" role="status">
          <Icon name="alert" size="sm" />
          <span>
            {left()
              ? t("This app tried to leave its page, so Haarness closed it.")
              : t("This app's page could not be loaded.")}
            <Show when={failure()}>
              <span class="notice-detail">{failure()}</span>
            </Show>
          </span>
          <button
            class="mcp-app-action"
            onClick={() => {
              if (failure()) load();
              setLeft(false);
              setHostReady(false);
              setRender((value) => value + 1);
            }}
          >
            {t("Try again")}
          </button>
        </div>
      </Show>
      <Show when={props.payload.tooLarge}>
        <div class="notice-inline mcp-app-notice" role="status">
          <Icon name="alert" size="sm" />
          <span>{t("This result is too big to display here.")}</span>
        </div>
      </Show>
      <Show when={refused() !== undefined}>
        <div class="notice-inline mcp-app-notice mcp-app-refused" role="status">
          <Icon name="alert" size="sm" />
          <span>
            {refused()
              ? t("This app tried to open a {scheme} link. Apps may open only http, https and mailto links.", {
                  scheme: refused()!,
                })
              : t("This app tried to open a link Haarness does not open. Apps may open only http, https and mailto links.")}
          </span>
          <button class="mcp-app-action" onClick={() => setRefused(undefined)}>
            {t("Dismiss")}
          </button>
        </div>
      </Show>
      <Show when={message()}>
        {(pending) => (
          <div class="notice-inline mcp-app-request" role="group" aria-label={t("Message from the app")}>
            <Icon name="chat" size="sm" />
            <span>
              {t("This app wants to write a message for you.")}
              <span class="notice-detail mcp-app-message">{pending().text}</span>
            </span>
            <button class="mcp-app-action" onClick={() => pending().answer(true)}>
              {t("Insert app message")}
            </button>
            <button class="mcp-app-action" onClick={() => pending().answer(false)}>
              {t("Dismiss")}
            </button>
          </div>
        )}
      </Show>
      <Show when={request()}>
        {(pending) => (
          <div class="notice-inline mcp-app-request" role="group" aria-label={t("Link from the app")}>
            <Icon name="external" size="sm" />
            <span>
              {t("This app wants to open a link.")}
              <span class="notice-detail mcp-app-url">{pending().url}</span>
            </span>
            <button class="mcp-app-action" onClick={() => pending().answer(true)}>
              {t("Open")}
            </button>
            <button class="mcp-app-action" onClick={() => pending().answer(false)}>
              {t("Dismiss")}
            </button>
          </div>
        )}
      </Show>
    </div>
  );
}

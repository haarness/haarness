import { t } from "./i18n";
import { browserAddress } from "../browser-settings";
import {
  createEffect,
  createSignal,
  For,
  Show,
  onMount,
  onCleanup,
  untrack,
} from "solid-js";
import { Icon } from "./ui";
import { useApp } from "./state";
import type { BrowserState } from "../shared";
import { setNativeArea } from "./native-area";
import { CHROME_CAPABILITY, chromeThreadTabs, type ChromeHosts } from "../chrome";
import "./browser.css";
const empty = (): BrowserState => ({ tabs: [], focusedTabID: null });
/** Everything that floats above the page: menus, pickers, popovers, dialogs, toasts. */
const FLOATING = [
  "dialog[open]",
  '[role="dialog"]',
  '[role="alertdialog"]',
  '[role="menu"]',
  '[role="listbox"]',
  '[role="tooltip"]',
  ".popover",
  ".select-popover",
  ".project-popover",
  ".search-overlay",
  ".modal-backdrop",
  ".notifications > *",
].join(", ");
/** Whether a floating layer paints where the native page is drawn. */
function covered(area: DOMRect) {
  for (const layer of document.querySelectorAll<HTMLElement>(FLOATING)) {
    // A modal's backdrop dims the whole window, including the page.
    if (layer.matches("dialog:modal")) return true;
    if (!layer.checkVisibility({ opacityProperty: true, visibilityProperty: true }))
      continue;
    const box = layer.getBoundingClientRect();
    if (
      box.width > 0 &&
      box.height > 0 &&
      box.left < area.right &&
      box.right > area.left &&
      box.top < area.bottom &&
      box.bottom > area.top
    )
      return true;
  }
  return false;
}
export function BrowserPanel(props: { visible: boolean; onOpen: () => void }) {
  const app = useApp();
  const { state } = app;
  const [editingAddress, setEditingAddress] = createSignal(false);
  const [browser, setBrowser] = createSignal(empty());
  const [address, setAddress] = createSignal("");
  const [error, setError] = createSignal("");
  const [connected, setConnected] = createSignal(false);
  // Tabs this chat holds in the user's Chrome, which this panel cannot show.
  const [chromeTabs, setChromeTabs] = createSignal(0);
  // The native page draws above the DOM, so a layer that must cover it gets
  // a still frame of the page to paint over instead of a blank hole.
  const [still, setStill] = createSignal<string>();
  let held = false;
  let freezing = 0;
  let frames = 0;
  let stillImage: HTMLImageElement | undefined;
  let viewport!: HTMLDivElement;
  let epoch = 0;
  let lastLayout = "";
  let retry: ReturnType<typeof setTimeout> | undefined;
  const tab = () =>
    browser().tabs.find((tab) => tab.id === browser().focusedTabID);
  const owner = () => ({
    connection: state.connection?.id ?? "",
    thread: state.selected ?? "",
  });
  const belongs = (connection: string, thread: string) =>
    connection === owner().connection && thread === owner().thread;
  async function attach(connection: string, thread: string, version: number) {
    if (!connection) return;
    try {
      const value = await window.haarness.browserAttach(connection, thread);
      if (version !== epoch) return;
      setBrowser(value);
      setConnected(true);
      setError("");
      updateLayout();
    } catch (reason) {
      if (version !== epoch) return;
      setConnected(false);
      setError(String(reason));
      retry = setTimeout(() => void attach(connection, thread, version), 5000);
    }
  }
  createEffect(() => {
    const { connection, thread } = owner();
    const version = ++epoch;
    lastLayout = "";
    held = false;
    freezing = 0;
    setStill(undefined);
    clearTimeout(retry);
    setEditingAddress(false);
    setAddress("");
    setBrowser(empty());
    setConnected(false);
    setError("");
    void attach(connection, thread, version);
  });
  // While the panel shows, ask now and then whether the chat also works in
  // Chrome. Servers without Chrome support are never asked.
  createEffect(() => {
    const { connection, thread } = owner();
    setChromeTabs(0);
    if (
      !props.visible ||
      !thread ||
      !state.connection?.info.capabilities.includes(CHROME_CAPABILITY)
    )
      return;
    let alive = true;
    const load = () =>
      void window.haarness
        .api<ChromeHosts>(connection, "listBrowserHosts", [{ thread }])
        .then((value) => {
          if (alive) setChromeTabs(chromeThreadTabs(value.hosts ?? []));
        })
        .catch(() => {});
    load();
    const timer = setInterval(load, 5000);
    onCleanup(() => {
      alive = false;
      clearInterval(timer);
    });
  });
  let addressTab: string | undefined;
  let addressURL = "";
  // The page an address was submitted over. The host keeps reporting that
  // page while the next one loads, and a tab opened for the address starts
  // blank, so the field keeps what was typed until the tab leaves that page
  // or stops loading, then shows where it landed.
  let submitted: { tab?: string; url: string; loading: boolean } | undefined;
  createEffect(() => {
    const current = tab();
    const url = current?.url ?? "";
    if (submitted) {
      const same = current?.id === submitted.tab && url === submitted.url;
      const opening = !submitted.tab && (!url || url === "about:blank");
      if (same && current?.loading) submitted.loading = true;
      if (opening || (same && (current?.loading || !submitted.loading)))
        return;
    }
    submitted = undefined;
    if (current?.id === addressTab && url === addressURL) return;
    addressTab = current?.id;
    addressURL = url;
    // Host state and loading events must not overwrite a URL being typed,
    // even for a new tab that arrives while it is typed.
    if (!untrack(editingAddress)) setAddress(url);
  });
  createEffect(() => {
    props.visible;
    browser().focusedTabID;
    queueMicrotask(updateLayout);
  });
  function updateLayout() {
    if (!viewport) return;
    const { connection, thread } = owner();
    const bounds = viewport.getBoundingClientRect();
    const live =
      props.visible && connected() && bounds.width > 0 && bounds.height > 0;
    setNativeArea(live && tab() ? bounds : undefined);
    if (live && covered(bounds)) {
      if (!held) freeze(connection, thread);
    } else {
      freezing = 0;
      held = false;
    }
    const layout = {
      visible: live && !held,
      bounds: {
        x: bounds.x,
        y: bounds.y,
        width: bounds.width,
        height: bounds.height,
      },
    };
    const identity = JSON.stringify([connection, thread, layout]);
    if (identity === lastLayout) return;
    lastLayout = identity;
    const thawed = !held && !!still();
    void window.haarness
      .browserLayout(connection, thread, layout)
      .then(() => {
        // Drop the still only once the live page is back on top of it.
        const frame = ++frames;
        if (thawed)
          requestAnimationFrame(() => {
            if (frame === frames && !held && !freezing) setStill(undefined);
          });
      })
      .catch(() => {
        lastLayout = "";
      });
  }
  /** Swap the live page for a still frame, then let the layer above paint. */
  function freeze(connection: string, thread: string) {
    if (freezing) return;
    const version = (freezing = ++frames);
    const current = () => freezing === version && belongs(connection, thread);
    void window.haarness
      .browserSnapshot(connection, thread)
      .catch(() => null)
      .then(async (image) => {
        if (!current()) return;
        if (image) {
          setStill(image);
          // Hide the live page only once the still is ready to paint.
          await stillImage?.decode().catch(() => {});
          await new Promise(requestAnimationFrame);
          if (!current()) return;
        }
        freezing = 0;
        held = true;
        updateLayout();
      });
  }
  async function command(action: unknown) {
    const { connection, thread } = owner();
    try {
      setError("");
      await window.haarness.browserCommand(connection, thread, action);
    } catch (reason) {
      if (belongs(connection, thread)) setError(String(reason));
    }
  }
  onMount(() => {
    const stop = window.haarness.onEvent((event) => {
      if (
        event.kind !== "browser" ||
        !belongs(event.connection ?? "", event.id ?? "")
      )
        return;
      const data = event.data as {
        state?: BrowserState;
        error?: string;
        focus?: boolean;
        disconnected?: boolean;
      };
      if (data.state) setBrowser(data.state);
      if (data.error) setError(data.error);
      if (data.focus) props.onOpen();
      if (data.disconnected) {
        setConnected(false);
        clearTimeout(retry);
        const { connection, thread } = owner();
        const version = epoch;
        retry = setTimeout(
          () => void attach(connection, thread, version),
          5000,
        );
      }
      queueMicrotask(updateLayout);
    });
    const resize = new ResizeObserver(updateLayout);
    resize.observe(viewport);
    const mutation = new MutationObserver(updateLayout);
    mutation.observe(document.body, {
      subtree: true,
      childList: true,
      attributes: true,
      attributeFilter: ["open", "style", "class"],
    });
    window.addEventListener("resize", updateLayout);
    document.addEventListener("scroll", updateLayout, true);
    onCleanup(() => {
      epoch++;
      stop();
      clearTimeout(retry);
      resize.disconnect();
      mutation.disconnect();
      window.removeEventListener("resize", updateLayout);
      document.removeEventListener("scroll", updateLayout, true);
      setNativeArea(undefined);
      const { connection, thread } = owner();
      void window.haarness.browserLayout(connection, thread, {
        visible: false,
        bounds: { x: 0, y: 0, width: 0, height: 0 },
      });
    });
  });
  return (
    <section
      class="browser-panel"
      style={{ display: props.visible ? "flex" : "none" }}
      aria-label={t("Web browser")}
    >
      <div class="browser-tabs" role="tablist" aria-label={t("Browser tabs")}>
        <For each={browser().tabs}>
          {(item) => (
            <div
              class="browser-tab"
              classList={{ active: item.id === browser().focusedTabID }}
            >
              <button
                class="browser-tab-title"
                role="tab"
                aria-selected={item.id === browser().focusedTabID}
                title={
                  item.title && item.url
                    ? item.title + "\n" + item.url
                    : item.url
                }
                onClick={() =>
                  void command({ type: "tabs.focus", tabID: item.id })
                }
              >
                {/* Like any browser: the site's icon, a spinner in its place
                    while loading, and the title read from its start. */}
                <span class="browser-tab-icon" aria-hidden="true">
                  <Show
                    when={!item.loading}
                    fallback={<span class="browser-tab-spinner" />}
                  >
                    <Show when={item.favicon} fallback={<Icon name="web" size="sm" />}>
                      {(src) => <img src={src()} alt="" draggable={false} />}
                    </Show>
                  </Show>
                </span>
                <span class="browser-tab-label">
                  {item.title || item.url || t("New tab")}
                </span>
              </button>
              <button
                class="browser-tab-close"
                aria-label={t("Close browser tab {name}", {
                  name: item.title || item.url || t("New tab"),
                })}
                onClick={() =>
                  void command({ type: "tabs.close", tabID: item.id })
                }
              >
                <Icon name="close" size="sm" />
              </button>
            </div>
          )}
        </For>
        <button
          class="browser-new"
          aria-label={t("New browser tab")}
          disabled={!state.connection}
          onClick={() => void command({ type: "tabs.open" })}
        >
          <Icon name="plus" size="sm" />
        </button>
      </div>
      <form
        class="browser-toolbar"
        onSubmit={(event) => {
          event.preventDefault();
          const current = tab();
          const url = address();
          // Like a browser, Enter hands focus to the page and the field then
          // shows wherever it lands, even the same URL written another way.
          addressURL = "";
          submitted = { tab: current?.id, url: current?.url ?? "", loading: false };
          const field = event.currentTarget.querySelector("input");
          if (field && field === document.activeElement) field.blur();
          if (current)
            void command({ type: "navigate", tabID: current.id, url });
          else void command({ type: "tabs.open", url });
        }}
      >
        <button
          type="button"
          aria-label={t("Browser back")}
          disabled={!tab()?.canGoBack}
          onClick={() => void command({ type: "back", tabID: tab()?.id })}
        >
          <Icon name="back" size="sm" />
        </button>
        <button
          type="button"
          aria-label={t("Browser forward")}
          disabled={!tab()?.canGoForward}
          onClick={() => void command({ type: "forward", tabID: tab()?.id })}
        >
          <Icon name="forward" size="sm" />
        </button>
        <button
          type="button"
          aria-label={t("Reload browser page")}
          disabled={!tab()}
          onClick={() => void command({ type: "reload", tabID: tab()?.id })}
        >
          <Icon name="refresh" size="sm" />
        </button>
        <input
          aria-label={t("Browser address")}
          placeholder={t("Enter a URL")}
          value={editingAddress() ? address() : browserAddress(address(), state.preferences.browserShowFullURL)}
          onFocus={(event) => { setEditingAddress(true); setAddress(tab()?.url ?? address()); event.currentTarget.select(); }}
          onBlur={() => setEditingAddress(false)}
          onInput={(event) => setAddress(event.currentTarget.value)}
        />
        <button type="button" aria-label={t("Browser settings")} onClick={() => void app.openSettings("browser")}><Icon name="settings" size="sm" /></button>
        <button type="submit" disabled={!state.connection}>
          {t("Go")}
        </button>
      </form>
      <Show when={error()}>
        <div class="browser-error" role="status">
          {error()}{" "}
          <button
            onClick={() => {
              const { connection, thread } = owner();
              void attach(connection, thread, epoch);
            }}
          >
            {t("Reconnect")}
          </button>
        </div>
      </Show>
      <div class="browser-viewport" ref={viewport}>
        <Show when={still()}>
          <img
            ref={stillImage}
            class="browser-still"
            src={still()}
            alt=""
            aria-hidden="true"
            draggable={false}
          />
        </Show>
        <Show when={!tab()}>
          <div class="browser-empty">
            <strong>{t("Browse alongside your work")}</strong>
            <p>
              {state.selected
                ? t("Open a tab or ask Haarness to browse.")
                : t("Open a tab now. It will follow you into your next chat.")}
            </p>
            <small>
              {state.selected ? t("Each chat has its own browser.") : t("Tabs follow you into your next chat.")}{" "}
              {t("Localhost uses this project’s server.")}
            </small>
          </div>
        </Show>
      </div>
      {/* Only what changes how browsing behaves: a lost connection, a
          remote server whose network (and localhost) the pages use, or tabs
          this chat keeps in the user's Chrome. */}
      <Show when={!connected() || state.connection?.remote || chromeTabs()}>
        <div class="browser-status" role="status">
          <Show when={!connected() || state.connection?.remote}>
            <p>
              {!connected()
                ? t("Browser disconnected. It reconnects automatically.")
                : t("Pages load through the remote server, so localhost is that machine.")}
            </p>
          </Show>
          <Show when={chromeTabs()}>
            <p>
              {chromeTabs() === 1
                ? t("This chat also uses Chrome · 1 tab")
                : t("This chat also uses Chrome · {count} tabs", {
                    count: chromeTabs(),
                  })}
            </p>
          </Show>
        </div>
      </Show>
    </section>
  );
}

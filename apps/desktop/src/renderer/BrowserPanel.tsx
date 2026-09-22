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
import "./browser.css";
const empty = (): BrowserState => ({ tabs: [], focusedTabID: null });
export function BrowserPanel(props: { visible: boolean; onOpen: () => void }) {
  const app = useApp();
  const { state } = app;
  const [editingAddress, setEditingAddress] = createSignal(false);
  const [browser, setBrowser] = createSignal(empty());
  const [address, setAddress] = createSignal("");
  const [error, setError] = createSignal("");
  const [connected, setConnected] = createSignal(false);
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
    clearTimeout(retry);
    setEditingAddress(false);
    setAddress("");
    setBrowser(empty());
    setConnected(false);
    setError("");
    void attach(connection, thread, version);
  });
  let addressTab: string | undefined;
  let addressURL = "";
  createEffect(() => {
    const current = tab();
    const url = current?.url ?? "";
    if (current?.id === addressTab && url === addressURL) return;
    addressTab = current?.id;
    addressURL = url;
    // Host state and loading events must not overwrite a URL being typed.
    if (!untrack(editingAddress)) setAddress(url);
  });
  createEffect(() => {
    props.visible;
    state.settings;
    browser().focusedTabID;
    queueMicrotask(updateLayout);
  });
  function updateLayout() {
    if (!viewport) return;
    const { connection, thread } = owner();
    const bounds = viewport.getBoundingClientRect();
    const overlay = !!document.querySelector(
      'dialog[open], [role="dialog"], [role="menu"], .menu-popover, .search-overlay',
    );
    const layout = {
      visible:
        props.visible &&
        connected() &&
        !state.settings &&
        !overlay &&
        bounds.width > 0 &&
        bounds.height > 0,
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
    void window.haarness.browserLayout(connection, thread, layout).catch(() => {
      lastLayout = "";
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
              <Show when={item.loading}>
                <span class="browser-tab-busy" aria-hidden="true">
                  ◌
                </span>
              </Show>
              <button
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
                {item.title || item.url || t("New tab")}
              </button>
              <button
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
          if (current)
            void command({
              type: "navigate",
              tabID: current.id,
              url: address(),
            });
          else void command({ type: "tabs.open", url: address() });
        }}
      >
        <button
          type="button"
          aria-label={t("Browser back")}
          disabled={!tab()?.canGoBack}
          onClick={() => void command({ type: "back", tabID: tab()?.id })}
        >
          ←
        </button>
        <button
          type="button"
          aria-label={t("Browser forward")}
          disabled={!tab()?.canGoForward}
          onClick={() => void command({ type: "forward", tabID: tab()?.id })}
        >
          →
        </button>
        <button
          type="button"
          aria-label={t("Reload browser page")}
          disabled={!tab()}
          onClick={() => void command({ type: "reload", tabID: tab()?.id })}
        >
          ↻
        </button>
        <input
          aria-label={t("Browser address")}
          placeholder={t("Enter a URL")}
          value={editingAddress() ? address() : browserAddress(address(), state.preferences.browserShowFullURL)}
          onFocus={(event) => { setEditingAddress(true); setAddress(tab()?.url ?? address()); event.currentTarget.select(); }}
          onBlur={() => setEditingAddress(false)}
          onInput={(event) => setAddress(event.currentTarget.value)}
        />
        <button type="button" aria-label={t("Browser settings")} onClick={() => void app.openSettings("browser")}>⚙</button>
        <button type="submit" disabled={!state.connection}>
          Go
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
        <Show when={!tab()}>
          <div class="browser-empty">
            <strong>{t("Browse alongside your work")}</strong>
            <p>
              {state.selected
                ? "Open a tab or ask Haarness to browse."
                : "Open a tab now. It will follow you into your next chat."}
            </p>
            <small>
              {state.selected ? "Each conversation has its own browser." : "Tabs follow you into your next chat."} Localhost uses this
              project’s server.
            </small>
          </div>
        </Show>
      </div>
      <div class="browser-status">
        {connected()
          ? state.selected ? "Connected to this conversation" : "Ready for your next chat"
          : "Browser disconnected"}{" "}
        ·{" "}
        {state.connection?.remote
          ? "Remote server network"
          : "Local server network"}
      </div>
    </section>
  );
}

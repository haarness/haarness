import { t } from "./i18n";
import {
  For,
  Show,
  createSignal,
  createEffect,
  onMount,
  onCleanup,
} from "solid-js";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { sizePusher } from "./terminal-size";
import {
  inputQueue,
  linkWatch,
  reconnectDelay,
  shownOutput,
  terminalStream,
  writeChunks,
  type Link,
} from "./terminal-stream";
import "@xterm/xterm/css/xterm.css";
import type { PtyInfo } from "@haarness/sdk";
import { useApp } from "./state";
import { Button, Icon, ask } from "./ui";
import { createOrdering } from "./Sortable";
import { registerCyclePanel } from "./tab-cycle";

// A quick hide/reopen can remount the panel before shell creation returns.
// Share startup by connection so that gesture still opens exactly one shell.
const openings = new Map<string, Promise<void>>();
async function ensureTerminal(connection: string) {
  let pending = openings.get(connection);
  if (!pending) {
    pending = (async () => {
      const tabs = await window.haarness.api<PtyInfo[]>(
        connection,
        "listPtys",
        [],
      );
      if (!tabs.length) {
        const item = await window.haarness.api<PtyInfo>(connection, "createPty", [
          { cols: 100, rows: 20 },
        ]);
        shownOutput.started(connection + ":" + item.id);
      }
    })();
    openings.set(connection, pending);
  }
  try {
    await pending;
  } finally {
    if (openings.get(connection) === pending) openings.delete(connection);
  }
}

export function TerminalPanel() {
  const app = useApp();
  const [tabs, setTabs] = createSignal<PtyInfo[]>([]),
    [selected, setSelected] = createSignal("");
  const [starting, setStarting] = createSignal(true);
  const [creating, setCreating] = createSignal(false);
  const [closing, setClosing] = createSignal<string[]>([]);
  const order = createOrdering(
    () => "list:terminals:" + app.state.connection?.id,
    () => tabs().map((tab) => tab.id),
  );
  let generation = 0;
  let disposed = false;
  async function refresh(open = false) {
    const id = ++generation;
    const connection = app.state.connection!.id;
    try {
      if (open) await ensureTerminal(connection);
      const values = await window.haarness.api<PtyInfo[]>(
        connection,
        "listPtys",
        [],
      );
      if (id === generation && connection === app.state.connection?.id) {
        shownOutput.keep(connection + ":", values.map((value) => value.id));
        setTabs(values);
        if (!values.some((value) => value.id === selected()))
          setSelected(values[0]?.id ?? "");
      }
      return values;
    } finally {
      if (id === generation) setStarting(false);
    }
  }
  async function create() {
    if (starting() || creating()) return;
    const connection = app.state.connection!.id;
    setCreating(true);
    try {
      const item = await window.haarness.api<PtyInfo>(connection, "createPty", [
        { cols: 100, rows: 20 },
      ]);
      shownOutput.started(connection + ":" + item.id);
      if (disposed || connection !== app.state.connection?.id) return;
      await refresh();
      if (!disposed && connection === app.state.connection?.id)
        setSelected(item.id);
    } finally {
      setCreating(false);
    }
  }
  createEffect(() => {
    const connection = app.state.connection?.id;
    if (!connection) return;
    setTabs([]);
    setSelected("");
    setStarting(true);
    void app.run(() => refresh(true));
  });
  // A server that was replaced (restarted, or stopped and started again)
  // has none of the old terminals and sends no event for them: list them
  // again, which also opens a shell when none is left.
  let instance = { connection: "", id: "" };
  createEffect(() => {
    const connection = app.state.connection?.id ?? "";
    const id = app.state.connection?.info.serverInstanceId ?? "";
    const previous = instance;
    instance = { connection, id };
    if (connection && previous.connection === connection && previous.id && id !== previous.id)
      void app.run(() => refresh(true));
  });
  const off = window.haarness.onEvent((event) => {
    if (
      event.kind === "server" &&
      event.connection === app.state.connection?.id &&
      String((event.data as { type: string }).type).startsWith("pty.")
    )
      void app.run(() => refresh());
  });
  onCleanup(() => {
    disposed = true;
    generation++;
    off();
  });
  async function remove(tab: PtyInfo) {
    if (closing().includes(tab.id)) return;
    const connection = app.state.connection!.id;
    setClosing((ids) => [...ids, tab.id]);
    try {
      await window.haarness.api(connection, "deletePty", [tab.id]);
      if (disposed || connection !== app.state.connection?.id) return;
      const remaining = await refresh();
      if (
        !disposed &&
        connection === app.state.connection?.id &&
        !remaining.length &&
        !creating()
      )
        app.set("terminal", false);
    } finally {
      setClosing((ids) => ids.filter((id) => id !== tab.id));
    }
  }
  // Ctrl+Tab in the terminal steps its tabs, and the keyboard goes into the
  // terminal shown: its view is drawn as soon as the tab is selected, so the
  // next press is still the terminal's, connected or not.
  let panel!: HTMLElement;
  onCleanup(
    registerCyclePanel({
      id: "terminal",
      tabs: () => order.ordered(),
      active: () => selected(),
      select: setSelected,
      keep: () => panel.querySelector<HTMLElement>(".xterm-helper-textarea")?.focus(),
    }),
  );
  app.registerSearch("terminal", () => [
    {
      key: "terminal-new",
      group: "Terminal actions",
      name: t("New terminal"),
      icon: "plus",
      disabled: starting() || creating(),
      action: create,
    },
    ...tabs().map((tab) => ({
      key: "terminal-tab:" + tab.id,
      group: "Terminals",
      name: tab.title,
      icon: "terminal" as const,
      detail: app.state.connection?.workspace,
      action: () => setSelected(tab.id),
    })),
    ...(tabs().find((tab) => tab.id === selected())
      ? [
          {
            key: "terminal-close",
            group: "Terminal actions",
            name: t("Close terminal"),
            icon: "close" as const,
            detail: tabs().find((tab) => tab.id === selected())!.title,
            action: () => remove(tabs().find((tab) => tab.id === selected())!),
          },
        ]
      : []),
  ]);
  return (
    <section class="terminal-panel" data-cycle-panel="terminal" ref={panel}>
      <div class="terminal-tabs">
        <For each={order.ordered()}>
          {(id) => {
            const tab = () => tabs().find((item) => item.id === id)!;
            return (
              <div
                classList={{ active: id === selected() }}
                {...order.dropProps(
                  id,
                  t("terminal {name}", { name: tab().title }),
                  "x",
                )}
              >
                <button
                  data-cycle-tab={id}
                  aria-current={id === selected() ? "true" : undefined}
                  onClick={() => setSelected(id)}
                  onDblClick={() =>
                    app.run(async () => {
                      const value = await ask(t("Rename terminal"), [
                        { name: "title", label: t("Title"), value: tab().title },
                      ]);
                      if (value) {
                        await app.api("updatePty", id, {
                          title: value.title,
                        });
                        await refresh();
                      }
                    })
                  }
                >
                  {tab().title}
                  {tab().status === "exited" ? ` · ${t("exited")}` : ""}
                </button>
                <Button
                  icon="close"
                  label={t("Close terminal {name}", { name: tab().title })}
                  disabled={closing().includes(id)}
                  onClick={() => app.run(() => remove(tab()))}
                />
              </div>
            );
          }}
        </For>
        <Button
          icon="plus"
          label={t("New terminal")}
          disabled={starting() || creating()}
          onClick={() => app.run(create)}
        />
        <div class="grow" />
        <Button
          icon="close"
          label={t("Hide terminal")}
          hint={t("processes keep running")}
          onClick={() => app.set("terminal", false)}
        />
      </div>
      <Show
        when={selected()}
        keyed
        fallback={
          <div class="terminal-empty">
            <Show
              when={!starting()}
              fallback={<span role="status">{t("Opening terminal…")}</span>}
            >
              <span>{t("No terminal in this workspace.")}</span>
              <button onClick={() => app.run(create)}>{t("New terminal")}</button>
            </Show>
          </div>
        }
      >
        {(id) => (
          <TerminalView
            id={id}
            // The server answered without this terminal (it was replaced
            // while this view was cut off): show what it has instead.
            onGone={() => void app.run(() => refresh(true))}
            onNew={() => void app.run(create)}
          />
        )}
      </Show>
    </section>
  );
}
function TerminalView(props: { id: string; onGone: () => void; onNew: () => void }) {
  const app = useApp();
  const [link, setLink] = createSignal<Link>("connected");
  let retryNow = () => {};
  let host!: HTMLDivElement;
  let terminal: Terminal;
  let fit: FitAddon;
  let resize: ResizeObserver;
  let unsubscribe = () => {};
  let disposed = false;
  onMount(() => {
    const connection = app.state.connection!.id;
    terminal = new Terminal({
      fontFamily: "SFMono-Regular, Menlo, Consolas, monospace",
      fontSize: app.state.preferences.terminalFontSize,
      cursorBlink: true,
      scrollback: 5000,
      allowProposedApi: false,
    });
    fit = new FitAddon();
    terminal.loadAddon(fit);
    terminal.open(host);
    const applyTheme = () => {
      const style = getComputedStyle(document.documentElement);
      const color = (name: string) =>
        style.getPropertyValue("--" + name).trim();
      terminal.options.theme = {
        background: color("page"),
        foreground: color("text"),
        cursor: color("primary"),
        selectionBackground: color("selected"),
        black: color("page"),
        brightBlack: color("muted"),
        red: color("danger"),
        green: color("green"),
        yellow: color("syntax-number"),
        blue: color("syntax-property"),
        magenta: color("syntax-keyword"),
        cyan: color("primary"),
        white: color("text"),
        brightRed: color("danger"),
        brightGreen: color("green"),
        brightYellow: color("syntax-number"),
        brightBlue: color("syntax-property"),
        brightMagenta: color("syntax-keyword"),
        brightCyan: color("primary"),
        brightWhite: color("text"),
      };
    };
    applyTheme();
    const themeObserver = new MutationObserver(applyTheme);
    themeObserver.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ["style", "data-theme", "data-palette"],
    });
    onCleanup(() => themeObserver.disconnect());
    createEffect(() => {
      terminal.options.fontSize = app.state.preferences.terminalFontSize;
      fit.fit();
    });
    fit.fit();
    // This view's own connection: main ignores a late connect or detach of
    // a view of the same terminal that was replaced.
    const view = crypto.randomUUID();
    const key = connection + ":" + props.id;
    const stream = terminalStream({
      write: (bytes, done) => terminal.write(bytes, done),
      shown: () => shownOutput.get(key),
      show: (end) => shownOutput.set(key, end),
    });
    // What xterm sends while a key, mouse, focus or paste event is being
    // handled is the person at the terminal, and always goes to the shell;
    // xterm answers the output it parses later, in a task of its own.
    const typing = () => {
      const event = (window as { event?: Event }).event;
      return event instanceof UIEvent || event instanceof ClipboardEvent;
    };
    // Input while the connection is down waits for the reconnect.
    let live = false;
    let order = 0;
    const waiting = inputQueue();
    const send = (bytes: Uint8Array) => {
      const position = order++;
      if (!live) return waiting.add(position, bytes);
      void window.haarness
        .terminal(connection, "write", props.id, Array.from(bytes))
        .then((sent) => {
          if (sent !== false) return;
          live = false;
          waiting.add(position, bytes);
        })
        .catch((error) => app.set("error", String(error)));
    };
    const input = terminal.onData((data) => {
      if (!typing() && !stream.open()) return;
      // A long paste goes in several writes; IPC keeps them in order.
      for (const chunk of writeChunks(new TextEncoder().encode(data))) send(chunk);
    });
    // The PTY learns every size xterm takes: a panel resize, and a font size
    // change, which refits without the panel changing size. A push waits for
    // the size to settle and is skipped when the PTY already has that size.
    const sizes = sizePusher((size) => app.api("updatePty", props.id, size));
    const sized = terminal.onResize(({ cols, rows }) => sizes.schedule({ cols, rows }));
    // A new or reattached PTY may have any size; give it this view's.
    sizes.schedule({ cols: terminal.cols, rows: terminal.rows });
    let timer: ReturnType<typeof setTimeout>;
    resize = new ResizeObserver(() => {
      clearTimeout(timer);
      timer = setTimeout(() => fit.fit(), 100);
    });
    resize.observe(host);
    unsubscribe = window.haarness.onEvent((event) => {
      if (
        event.kind !== "pty" ||
        event.connection !== connection ||
        event.id !== props.id
      )
        return;
      const data = event.data as {
        bytes?: number[];
        type?: string;
        start?: number;
        end?: number;
        truncated?: boolean;
        modes?: string;
        view?: string;
      };
      if (data.view && data.view !== view) return;
      if (data.type === "replay" && typeof data.start === "number" && typeof data.end === "number") {
        shownOutput.adopt(key, data.end);
        stream.replay(Math.min(Math.max(requested, data.start), data.end));
      }
      if (data.bytes) stream.bytes(new Uint8Array(data.bytes));
      if (data.type === "cursor" && typeof data.end === "number") {
        stream.cursor(data.end);
        cursor = data.end;
        attempt = 0;
      }
      // Older output is gone, so the replay does not continue what the view
      // shows: start over (ESC c), say so, and put the terminal in the modes
      // the lost output left on, such as a full-screen program's alternate
      // screen, cursor keys, mouse reporting and bracketed paste.
      if (data.truncated && data.type === "replay")
        stream.quiet(
          new TextEncoder().encode(
            "\x1bc[" + t("Earlier terminal output is no longer available") + "]\r\n" +
              (data.modes ?? ""),
          ),
        );
      if (data.type === "closed") {
        live = false;
        watch.lost();
        reconnect();
      }
    });
    // The output this view has, as the server counts it. When the socket
    // ends (the server drops a reader that fell behind, or the network or
    // server went away), the view connects again from there, so the shell
    // keeps its keys and the missed output is replayed once.
    let cursor = 0;
    let requested = 0;
    let attempt = 0;
    let retry: ReturnType<typeof setTimeout> | undefined;
    let focused = false;
    // What the view says while the connection is down; reconnects go on
    // whatever it says.
    const watch = linkWatch({ onState: setLink });
    const connect = () => {
      requested = cursor;
      void window.haarness
        .terminal(connection, "connect", props.id, { cursor, view })
        .then(() => {
          if (disposed) return;
          if (!focused) terminal.focus();
          focused = true;
          live = true;
          watch.connected();
          for (const bytes of waiting.take()) send(bytes);
          sizes.schedule({ cols: terminal.cols, rows: terminal.rows });
        })
        .catch(() => {
          if (disposed) return;
          watch.lost();
          reconnect();
        });
    };
    function reconnect(delay = reconnectDelay(attempt++)) {
      if (disposed) return;
      clearTimeout(retry);
      retry = setTimeout(async () => {
        // While the server cannot say, keep trying. A shell that exited
        // sends nothing more, and its tab says so; a terminal the server no
        // longer has (it was replaced meanwhile) gives way to what it has.
        const tabs = await window.haarness
          .api<PtyInfo[]>(connection, "listPtys", [])
          .catch(() => undefined);
        if (disposed) return;
        if (!tabs) return reconnect();
        const tab = tabs.find((tab) => tab.id === props.id);
        if (tab?.status === "running") return connect();
        watch.connected();
        if (!tab) props.onGone();
      }, delay);
    }
    // Retry tries at once, and backs off from the start again.
    retryNow = () => {
      attempt = 0;
      watch.retry();
      reconnect(0);
    };
    connect();
    onCleanup(() => {
      disposed = true;
      watch.dispose();
      clearTimeout(retry);
      clearTimeout(timer);
      sizes.dispose();
      sized.dispose();
      input.dispose();
      resize.disconnect();
      unsubscribe();
      void window.haarness.terminal(connection, "detach", props.id, view);
      terminal.dispose();
    });
  });
  // xterm owns the view's children, so what the view says about its
  // connection sits beside it.
  return (
    <div class="terminal-stage" data-link={link()}>
      <div class="terminal-view" aria-label={t("Workspace terminal")} ref={host} />
      <Show when={link() === "reconnecting"}>
        <div class="terminal-reconnecting" role="status">
          <span class="spinner" />
          {t("Reconnecting…")}
        </div>
      </Show>
      <Show when={link() === "disconnected"}>
        <div class="terminal-disconnected">
          <div role="alert">
            <Icon name="alert" size="lg" />
            <strong>{t("Terminal disconnected")}</strong>
            <p>{t("Haarness can't reach this terminal. Its shell may still be running.")}</p>
            <div>
              <button type="button" onClick={() => props.onNew()}>
                {t("New terminal")}
              </button>
              <button type="button" class="primary" onClick={() => retryNow()}>
                {t("Retry")}
              </button>
            </div>
          </div>
        </div>
      </Show>
    </div>
  );
}

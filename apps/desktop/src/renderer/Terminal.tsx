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
import { pendingSize, type TerminalSize } from "./terminal-size";
import "@xterm/xterm/css/xterm.css";
import type { PtyInfo } from "@haarness/sdk";
import { useApp } from "./state";
import { Button, ask } from "./ui";
import { createOrdering } from "./Sortable";

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
      if (!tabs.length)
        await window.haarness.api(connection, "createPty", [
          { cols: 100, rows: 20 },
        ]);
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
    <section class="terminal-panel">
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
        {(id) => <TerminalView id={id} />}
      </Show>
    </section>
  );
}
function TerminalView(props: { id: string }) {
  const app = useApp();
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
    const input = terminal.onData(
      (data) =>
        void window.haarness
          .terminal(
            connection,
            "write",
            props.id,
            Array.from(new TextEncoder().encode(data)),
          )
          .catch((error) => app.set("error", String(error))),
    );
    // The PTY learns every size xterm takes: a panel resize, and a font size
    // change, which refits without the panel changing size. A push waits for
    // the size to settle and is skipped when the PTY already has that size.
    let pushed: TerminalSize | undefined;
    let pending: TerminalSize | undefined;
    let push: ReturnType<typeof setTimeout> | undefined;
    const schedule = (size: TerminalSize) => {
      pending = pendingSize(pushed, size);
      clearTimeout(push);
      if (!pending) return;
      push = setTimeout(() => {
        const size = pending;
        pending = undefined;
        if (!size || disposed) return;
        pushed = size;
        void app.api("updatePty", props.id, size).catch(() => {});
      }, 100);
    };
    const sized = terminal.onResize(({ cols, rows }) => schedule({ cols, rows }));
    // A new or reattached PTY may have any size; give it this view's.
    schedule({ cols: terminal.cols, rows: terminal.rows });
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
        truncated?: boolean;
        error?: string;
      };
      if (data.bytes) terminal.write(new Uint8Array(data.bytes));
      if (data.truncated && data.type === "replay")
        terminal.writeln(
          "\r\n[Earlier terminal output is no longer available]\r\n",
        );
      if (data.error) terminal.writeln("\r\n" + data.error);
    });
    void window.haarness
      .terminal(connection, "connect", props.id, 0)
      .then(() => {
        if (disposed) {
          void window.haarness.terminal(connection, "detach", props.id);
          return;
        }
        terminal.focus();
      })
      .catch((error) => app.set("error", String(error)));
    onCleanup(() => {
      disposed = true;
      clearTimeout(timer);
      clearTimeout(push);
      sized.dispose();
      input.dispose();
      resize.disconnect();
      unsubscribe();
      void window.haarness.terminal(connection, "detach", props.id);
      terminal.dispose();
    });
  });
  return (
    <div class="terminal-view" aria-label={t("Workspace terminal")} ref={host} />
  );
}

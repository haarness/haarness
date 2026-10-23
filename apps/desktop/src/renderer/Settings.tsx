import type { GlobalShortcut } from "../global-shortcut";
import { createSettingsSearch } from "./settings-search";
import { physicalKey, searchEntries, shortcutField, shortcutFromField, shortcutLabel } from "./search";
import { BaseTag, CurrentTag, PathText, SettingRow as Row } from "./SettingRow";
import { DiagnosticsSetting } from "./Diagnostics";
import { Markdown } from "./Markdown";
import { UsageStatisticsSetting } from "./UsageStatistics";
import { BrowserSettings } from "./BrowserSettings";
import { Notifications } from "./Notifications";
import { formatNumber, splitAround, t, languages } from "./i18n";
import { InputSettings } from "./InputSettings";
import { openServerConnection } from "./ServerConnect";
import { ManagementPanel } from "./ManagementPanel";
import { ProviderLabel } from "./ProviderIcon";
import { modelName } from "../model-names";
import { Schedules } from "./Schedules";
import { Hooks } from "./Hooks";
import { McpForm } from "./McpForm";
import { appsOff, loadReadOnlyServers, noteMcpServers, offersApps, readOnlyServer, setApps } from "./mcp-servers";
import { AssetForm, type AssetRow } from "./AssetForm";
import { definitionSource, OPEN_GROUP_LIMIT, toolGroupKey, toolGroups, toolSummary, toolTitle, type DefinitionRow } from "./settings-lists";
import { SourceForm } from "./SourceForm";
import { ProviderForm } from "./ProviderForm";
import { RouterConnectionDialog } from "./RouterConnectionDialog";
import { disconnectProvider } from "./ConnectProvider";
import { Select } from "./Select";
import { MenuButton } from "./MenuButton";
import {
  For,
  Match,
  Show,
  Switch,
  createSignal,
  createEffect,
  onCleanup,
  onMount,
} from "solid-js";
import { computers, lastSeen, loadComputers, platformName, watchComputers } from "./computers";
import type * as T from "@haarness/sdk";
import type { Preferences } from "../shared";
import { themes, themeColors } from "../themes";
import { useApp } from "./state";
import { dismissOnBackdrop } from "./ui";
import { Wordmark } from "./brand";
import { Button, Icon, ask, confirm, Empty } from "./ui";
import { restoreArgs, restoredMessage, restorePlan, restoreSummary } from "./profile-restore";
import {
  settingsSections as sections,
  settingsTitles as titles,
  shortcutDescriptions,
  shortcutTitles,
} from "./settings-catalog";
export function Settings() {
  const app = useApp();
  const search = createSettingsSearch(app);
  let content!: HTMLDivElement;
  let dialog!: HTMLDialogElement;
  onMount(() => dialog.showModal());
  onCleanup(() => dialog.close());
  createEffect(() => {
    const target = app.state.settingsTarget;
    app.state.settings;
    app.state.settingsRevision;
    setFilter("");
    if (!target) { queueMicrotask(() => content?.scrollTo({ top: 0 })); return; }
    let marked: HTMLElement | undefined;
    let disposed = false;
    let timer: ReturnType<typeof setTimeout>;
    const focus = () => {
      const element = content?.querySelector<HTMLElement>(
        `[data-setting="${CSS.escape(target)}"]`,
      );
      if (!element) return;
      if (marked !== element) {
        marked = element;
        element.classList.add("setting-search-target");
        element.scrollIntoView({ block: "center", behavior: "instant" });
        clearTimeout(timer);
        timer = setTimeout(
          () => element.classList.remove("setting-search-target"),
          2500,
        );
      }
      // The keyboard lands on the place's own action (`data-search-focus`),
      // the option a choice has chosen, or its first control that works.
      const usable = ":not(:disabled)";
      const control =
        element.querySelector<HTMLElement>(`[data-search-focus]${usable}`) ??
        element.querySelector<HTMLElement>(`[role="radio"][aria-checked="true"]${usable}`) ??
        element.querySelector<HTMLElement>(
          ["input", "button", "select", "textarea"].map((name) => name + usable).join(", "),
        );
      // Controls still disabled while the place loads are waited for.
      if (!control && element.querySelector("input, button, select, textarea")) {
        element.tabIndex = -1;
        if (document.activeElement !== element) element.focus({ preventScroll: true });
        return;
      }
      if (!control) element.tabIndex = -1;
      (control ?? element).focus({ preventScroll: true });
      observer.disconnect();
    };
    const observer = new MutationObserver(focus);
    queueMicrotask(() => {
      if (!disposed && content?.isConnected) {
        observer.observe(content, { childList: true, subtree: true, attributes: true, attributeFilter: ["disabled"] });
        focus();
      }
    });
    onCleanup(() => {
      disposed = true;
      observer.disconnect();
      clearTimeout(timer);
      marked?.classList.remove("setting-search-target");
    });
  });
  const [output, setOutput] = createSignal(""),
    [task, setTask] = createSignal(""),
    [filter, setFilter] = createSignal("");
  const off = window.haarness.onEvent((event) => {
    if (event.kind === "command" && event.id === task())
      setOutput((text) => (text + String(event.data)).slice(-100000));
  });
  onCleanup(off);
  async function cli(args: string[]) {
    const id = crypto.randomUUID();
    setTask(id);
    setOutput("");
    try {
      const value = await window.haarness.command(
        app.state.connection!.id,
        args,
        id,
      );
      setOutput(value.output);
      await app.catalogs();
      return value.value;
    } finally {
      setTask("");
    }
  }
  const connected = () => !!app.state.connection;
  const local = () => connected() && !app.state.connection!.remote;
  return (
    <dialog
      ref={dialog}
      class="settings settings-dialog"
      aria-label={t("Settings")}
      onCancel={(event) => { event.preventDefault(); void app.closeSettings(); }}
      {...dismissOnBackdrop(() => void app.closeSettings())}
    >
        <aside classList={{ "settings-searching": !!filter().trim() }}>
          <h2>
            {t("Settings")}<span class="brand-dot">.</span>
          </h2>
          <input
            class="settings-filter"
            aria-label={t("Filter settings")}
            placeholder={t("Filter…")}
            value={filter()}
            onInput={(event) => setFilter(event.currentTarget.value)}
          />
          <Show when={filter().trim()} fallback={
          <For each={sections}>
            {([group, items]) => {
              const visible = () =>
                items.filter((name) =>
                  `${titles[name]} ${t(titles[name])}`.toLowerCase().includes(filter().toLowerCase()),
                );
              return (
                <Show when={visible().length}>
                  <div class="settings-group">
                    <span class="section-label">{t(group)}</span>
                    <For each={visible()}>
                      {(name) => (
                        <button
                          classList={{ active: app.state.settings === name }}
                          onClick={async () => {
                            if (!(await app.canLeaveSettings())) return;
                            app.set("settingsTarget", null);
                            app.set("settings", name);
                            app.set("settingsRevision", (revision) => revision + 1);
                            setOutput("");
                          }}
                        >
                          {t(titles[name])}
                        </button>
                      )}
                    </For>
                  </div>
                </Show>
              );
            }}
          </For>
          }>
            <nav class="settings-search-results" aria-label={t("Settings search results")}>
              <For each={searchEntries(search(), filter())} fallback={<p class="muted">{t("No matching settings.")}</p>}>
                {(entry) => (
                  <button
                    title={entry.name}
                    onClick={() => void entry.action()}
                  >
                    <span>{entry.name}</span>
                    <small>{entry.detail}</small>
                  </button>
                )}
              </For>
            </nav>
          </Show>
          <span class="small muted">Haarness {app.state.version}</span>
        </aside>
        <div class="settings-main">
          <header>
            <div>
              <h1>{t(titles[app.state.settings ?? "general"])}</h1>
            </div>
            <Button
              icon="close"
              label={t("Close settings")}
              onClick={() => void app.closeSettings()}
            />
          </header>
          <div class="settings-content" ref={content}>
            <Show when={app.state.settings === "general"}>
              <General />
            </Show>
            <Show when={app.state.settings === "browser"}><BrowserSettings /></Show>
            <Show when={app.state.settings === "shortcuts"}>
              <Shortcuts />
            </Show>
            <Show when={app.state.settings === "servers"}>
              <Servers />
            </Show>
            <Show when={app.state.settings === "projects"}>
              <Projects />
            </Show>
            <Show when={app.state.settings === "about"}>
              <About />
            </Show>
            <Show
              when={
                ![
                  "general",
                  "browser",
                  "shortcuts",
                  "servers",
                  "projects",
                  "about",
                ].includes(app.state.settings ?? "")
              }
            >
              <Show
                when={connected()}
                fallback={
                  <Empty
                    title={t("Open project first.")}
                    detail={t("Runtime settings belong to the selected Haarness installation.")}
                  />
                }
              >
                <Show
                  when={
                    !local() &&
                    ["agents", "commands", "skills", "extensions"].includes(
                      app.state.settings!,
                    )
                  }
                >
                  <p class="notice-inline">
                    {t("This is a remote project. Runtime catalogs are available below; host configuration and credentials must be managed on that server.")}
                  </p>
                </Show>
                <Show when={app.state.settings === "providers" && local()}>
                  <Providers />
                </Show>
                <Show
                  when={
                    !local() &&
                    [
                      "providers",
                      "mcp",
                      "hooks",
                      "schedules",
                      "profiles",
                    ].includes(app.state.settings!)
                  }
                >
                  <ManagementPanel resource={app.state.settings!} rootHeading={false} />
                </Show>
                <Show when={app.state.settings === "marketplace"}>
                  <ManagementPanel resource="marketplace-ui" rootHeading={false} />
                </Show>
                <Show when={app.state.settings === "models"}>
                  <Models />
                </Show>
                <Show
                  when={["agents", "commands", "skills"].includes(
                    app.state.settings!,
                  )}
                >
                  <Assets kind={app.state.settings!} cli={cli} />
                </Show>
                <Show when={app.state.settings === "tools"}>
                  <Tools />
                </Show>
                <Show when={app.state.settings === "hooks" && local()}>
                  <Hooks />
                </Show>
                <Show when={app.state.settings === "mcp" && local()}>
                  <Mcp cli={cli} />
                </Show>
                <Show when={app.state.settings === "plugins"}>
                  <Plugins />
                </Show>
                <Show when={app.state.settings === "extensions" && local()}>
                  <Extensions cli={cli} />
                </Show>
                <Show when={app.state.settings === "schedules" && local()}>
                  <Schedules cli={cli} />
                </Show>
                <Show when={app.state.settings === "profiles" && local()}>
                  <Profiles cli={cli} />
                </Show>
              </Show>
            </Show>
            <Show
              when={app.state.settings !== "schedules" && (output() || task())}
            >
              <details class="command-output" open>
                <summary>
                  {t("Operation output")}
                  <Show when={task()}>
                    <button onClick={() => window.haarness.cancel(task())}>
                      {t("Cancel operation")}
                    </button>
                  </Show>
                </summary>
                <pre>{output() || t("Working…")}</pre>
                <For
                  each={[...output().matchAll(/https?:\/\/[^\s]+/g)].slice(
                    0,
                    4,
                  )}
                >
                  {(match) => (
                    <button
                      onClick={() => window.haarness.openExternal(match[0])}
                    >
                      {t("Open authorization page")}
                    </button>
                  )}
                </For>
              </details>
            </Show>
          </div>
        </div>
      <Notifications />
    </dialog>
  );
}
function General() {
  const app = useApp();
  const systemTheme = matchMedia("(prefers-color-scheme: dark)");
  const [systemDark, setSystemDark] = createSignal(systemTheme.matches);
  const systemChanged = () => setSystemDark(systemTheme.matches);
  systemTheme.addEventListener("change", systemChanged);
  onCleanup(() => systemTheme.removeEventListener("change", systemChanged));
  return (
    <>
      <Row title="Language" description="Language changes the interface only. English is the default.">
        <Select
          aria-label={t("Language")}
          value={app.state.preferences.language}
          options={languages.map(({ code, label }) => ({ value: code, label }))}
          onChange={(language) => app.run(() => app.settings({ language }))}
        />
      </Row>
      <InputSettings />
      <Row
        title="Appearance"
        description="Choose light, dark, or follow your system automatically."
      >
        <Select
          aria-label={t("Appearance")}
          value={app.state.preferences.theme}
          onChange={(value) =>
            app.run(() =>
              app.settings({ theme: value as Preferences["theme"] }),
            )
          }
          options={[
            { value: "light", label: t("Light") },
            { value: "system", label: t("System") },
            { value: "dark", label: t("Dark") },
          ]}
        />
      </Row>
      <div class="theme-section" data-setting="Theme">
        <div>
          <h3>{t("Theme")}</h3>
          <p>{t("Every palette works in light, dark, and automatic appearance.")}</p>
        </div>
        <div class="theme-grid" aria-label={t("Color themes")}>
          <For each={themes}>
            {(theme) => {
              const colors = () =>
                themeColors(
                  theme.id,
                  app.state.preferences.theme === "system"
                    ? systemDark()
                      ? "dark"
                      : "light"
                    : app.state.preferences.theme,
                );
              return (
                <button
                  class="theme-option"
                  aria-label={t("{name} theme", { name: t(theme.name) })}
                  aria-pressed={app.state.preferences.palette === theme.id}
                  title={t(theme.description)}
                  onClick={() =>
                    app.run(() => app.settings({ palette: theme.id }))
                  }
                >
                  <span
                    class="theme-preview"
                    style={{
                      background: colors().page,
                      "border-color": colors().line,
                    }}
                  >
                    <span
                      class="theme-preview-sidebar"
                      style={{ background: colors().sidebar }}
                    >
                      <i style={{ background: colors().primary }} />
                      <i style={{ background: colors().muted }} />
                      <i style={{ background: colors().muted }} />
                    </span>
                    <span class="theme-preview-chat">
                      <i style={{ background: colors().muted }} />
                      <i style={{ background: colors().muted }} />
                      <span style={{ background: colors().surface }}>
                        <b style={{ background: colors().primary }} />
                      </span>
                    </span>
                  </span>
                  <span class="theme-option-label">
                    {t(theme.name)}
                    <Show when={app.state.preferences.palette === theme.id}>
                      <Icon name="check" size="sm" />
                    </Show>
                  </span>
                </button>
              );
            }}
          </For>
        </div>
      </div>
      <Row
        title="Chat text size"
        description="Applies to messages and the composer. Use View → Zoom to scale the whole window."
      >
        <input
          aria-label={t("Text size")}
          type="range"
          min={11}
          max={24}
          value={app.state.preferences.fontSize}
          onInput={(event) =>
            app.run(() =>
              app.settings({ fontSize: Number(event.currentTarget.value) }),
            )
          }
        />
        <span>{app.state.preferences.fontSize}px</span>
      </Row>
      <Row title="Terminal text size">
        <input
          aria-label={t("Terminal text size")}
          type="number"
          min={10}
          max={24}
          value={app.state.preferences.terminalFontSize}
          onChange={(event) =>
            app.run(() =>
              app.settings({
                terminalFontSize: Number(event.currentTarget.value),
              }),
            )
          }
        />
      </Row>
      {/* Only macOS composes characters with Option; elsewhere Alt is Meta. */}
      <Show when={app.state.platform === "darwin"}>
        <Row
          title="Option as Meta key"
          description="Make Option send Meta (Esc) shortcuts in the terminal instead of typing special characters."
        >
          <Select
            aria-label={t("Option as Meta key")}
            value={app.state.preferences.terminalOptionAsMeta}
            onChange={(value) =>
              app.run(() =>
                app.settings({
                  terminalOptionAsMeta: value as Preferences["terminalOptionAsMeta"],
                }),
              )
            }
            options={[
              { value: "off", label: t("Off") },
              { value: "left", label: t("Left Option") },
              { value: "both", label: t("Both Option keys") },
            ]}
          />
        </Row>
      </Show>
      <For
        each={
          [
            [
              "notifications",
              "Task notifications",
              "Enable desktop notifications. Choose which alerts appear below.",
            ],
            [
              "compactTools",
              "Compact tool activity",
              "Keep commands and tool output folded until you open them.",
            ],
            [
              "editorWrap",
              "Wrap editor lines",
              "Keep long lines within the editor width.",
            ],
          ] as const
        }
      >
        {([key, title, description]) => (
          <Row title={title} description={description}>
            <input
              type="checkbox"
              role="switch"
              aria-label={t(title)}
              checked={!!app.state.preferences[key]}
              onChange={(event) =>
                app.run(() =>
                  app.settings({ [key]: event.currentTarget.checked }),
                )
              }
            />
          </Row>
        )}
      </For>
      <Row
        title="Thinking display"
        description="Compact keeps the model's thinking to one line that opens on click, Show writes it out in full, and Hide leaves it out."
      >
        <Select
          aria-label={t("Thinking display")}
          value={app.state.preferences.thinking}
          onChange={(value) =>
            app.run(() =>
              app.settings({ thinking: value as Preferences["thinking"] }),
            )
          }
          options={[
            { value: "compact", label: t("Compact") },
            { value: "show", label: t("Show") },
            { value: "hide", label: t("Hide") },
          ]}
        />
      </Row>
      <Row title="Turn completion notifications" description="Choose when completed or failed tasks notify you.">
        <Select aria-label={t("Turn completion notifications")} value={app.state.preferences.completionNotifications}
          options={[{ value: "background", label: t("Only when unfocused") }, { value: "always", label: t("Always") }, { value: "never", label: t("Never") }]}
          onChange={(value) => app.run(() => app.settings({ completionNotifications: value as Preferences["completionNotifications"] }))} />
      </Row>
      <For each={[["permissionNotifications", "Permission notifications", "Notify when approval is needed while the app is in the background."], ["questionNotifications", "Question notifications", "Notify when input is needed while the app is in the background."]] as const}>
        {([key, title, description]) => <Row title={title} description={description}><input type="checkbox" role="switch" aria-label={t(title)} checked={app.state.preferences[key]} onChange={(event) => app.run(() => app.settings({ [key]: event.currentTarget.checked }))} /></Row>}
      </For>
      <Row title="Send message with">
        <Select
          aria-label={t("Send message with")}
          value={app.state.preferences.sendKey}
          onChange={(value) =>
            app.run(() =>
              app.settings({ sendKey: value as Preferences["sendKey"] }),
            )
          }
          options={[
            { value: "enter", label: t("Enter") },
            { value: "mod-enter", label: t("Command / Control + Enter") },
          ]}
        />
      </Row>
      <UsageStatisticsSetting />
      <DiagnosticsSetting />
      <p class="muted">
        {t("Drafts, appearance and saved projects are saved on this computer. Chats and tasks are saved separately for each project folder or worktree.")}
      </p>
    </>
  );
}
/**
 * A chord as a shortcut field records it, so nobody has to spell
 * "Mod+Shift+E": undefined until a key other than a modifier is pressed with
 * Command, Control or Option. On macOS Control is a key of its own
 * (Ctrl+Tab); elsewhere it is Mod.
 */
function recordChord(event: KeyboardEvent, platform: string) {
  const mac = platform === "darwin";
  const mod = mac ? event.metaKey : event.ctrlKey;
  const ctrl = mac && event.ctrlKey;
  if ((!mod && !ctrl && !event.altKey) || ["Meta", "Control", "Shift", "Alt"].includes(event.key))
    return;
  const key = physicalKey(event.code)
    ?? (event.code === "Space" ? "Space" : event.key.length === 1 ? event.key.toUpperCase() : event.key);
  return [
    mod && "Mod",
    ctrl && "Ctrl",
    event.altKey && "Alt",
    event.shiftKey && "Shift",
    key,
  ].filter(Boolean).join("+");
}
/** Show and hide Haarness from any app: a switch, off by default, and the
 * combination, recorded like the other shortcuts. */
function GlobalShortcutRows() {
  const app = useApp();
  const value = () => app.state.preferences.globalShortcut;
  let field!: HTMLInputElement;
  // Settings closing with the field focused sends no blur.
  let recording = false;
  const resume = () => {
    if (!recording) return;
    recording = false;
    void window.haarness.globalShortcut("resume");
  };
  onCleanup(resume);
  // A refused combination leaves the saved one in the field.
  const save = (next: GlobalShortcut) =>
    app.run(async () => {
      try {
        await app.settings({ globalShortcut: next });
      } finally {
        field.value = shortcutField(value().keys, app.state.platform);
      }
    });
  return (
    <>
      <Row
        title="Show and hide Haarness from any app"
        description="Brings Haarness to the front from any app, and hides it again when it is in front. While it is on, other apps no longer receive the combination."
      >
        <input
          type="checkbox"
          role="switch"
          aria-label={t("Show and hide Haarness from any app")}
          checked={value().enabled}
          onChange={(event) => {
            const enabled = event.currentTarget.checked;
            event.currentTarget.checked = value().enabled;
            void save({ ...value(), enabled });
          }}
        />
      </Row>
      <Row
        title="Global shortcut"
        description="Click and press the keys, including a modifier key."
      >
        <kbd class="shortcut-preview" dir="ltr" aria-hidden="true">
          {shortcutLabel(value().keys, app.state.platform)}
        </kbd>
        <input
          ref={field}
          class="shortcut-input"
          aria-label={t("Global shortcut")}
          value={shortcutField(value().keys, app.state.platform)}
          disabled={!value().enabled}
          onFocus={() => {
            recording = true;
            void window.haarness.globalShortcut("suspend");
          }}
          onBlur={resume}
          onKeyDown={(event) => {
            const chord = recordChord(event, app.state.platform);
            if (!chord) return;
            event.preventDefault();
            event.currentTarget.value = shortcutField(chord, app.state.platform);
            if (chord !== value().keys) void save({ ...value(), keys: chord });
          }}
          onChange={(event) => {
            const keys = shortcutFromField(event.currentTarget.value.trim(), app.state.platform);
            if (keys !== value().keys) void save({ ...value(), keys });
          }}
        />
      </Row>
    </>
  );
}
function Shortcuts() {
  const app = useApp();
  return (
    <>
      <p class="muted">
        {t("Click a shortcut and press the keys you want. The editor also supports standard search, selection, undo, and save shortcuts.")}
      </p>
      <For each={Object.entries(app.state.preferences.shortcuts)}>
        {([name, value]) => (
          <Row
            title={shortcutTitles[name] ?? name}
            target={"shortcut:" + name}
            description={shortcutDescriptions[name]}
          >
            <kbd class="shortcut-preview" dir="ltr" aria-hidden="true">
              {shortcutLabel(value, app.state.platform)}
            </kbd>
            <input
              class="shortcut-input"
              aria-label={t("Shortcut {name}", { name })}
              value={shortcutField(value, app.state.platform)}
              onKeyDown={(event) => {
                const chord = recordChord(event, app.state.platform);
                if (!chord) return;
                event.preventDefault();
                event.currentTarget.value = shortcutField(chord, app.state.platform);
                event.currentTarget.dispatchEvent(new Event("change"));
              }}
              onChange={(event) =>
                app.run(() =>
                  app.settings({
                    shortcuts: {
                      ...app.state.preferences.shortcuts,
                      [name]: shortcutFromField(event.currentTarget.value, app.state.platform),
                    },
                  }),
                )
              }
            />
          </Row>
        )}
      </For>
      <GlobalShortcutRows />
      <Row title="New line">
        <kbd dir="ltr">{shortcutLabel("Shift+Enter", app.state.platform)}</kbd>
      </Row>
      <Row title="Save file">
        <kbd dir="ltr">{shortcutLabel("Mod+S", app.state.platform)}</kbd>
      </Row>
      <Row title="Find in file">
        <kbd dir="ltr">{shortcutLabel("Mod+F", app.state.platform)}</kbd>
      </Row>
      <Row title="Close dialog">
        <kbd>{t("Escape")}</kbd>
      </Row>
    </>
  );
}
function Servers() {
  const app = useApp();
  return (
    <>
      <p class="muted">
        {t("Select a server to see its projects and chats.")}
      </p>
      <For each={app.servers().filter((item) => !item.host)}>
        {(item) => (
          <Row
            title={item.label}
            target={"server:" + item.id}
            tag={app.state.activeServer === item.id ? <CurrentTag /> : undefined}
            description={
              item.url ??
              (item.cloud
                ? "Cloud sessions in your Haarness account"
                : "Local projects on this computer")
            }
          >
            {/* The server shown says so beside its name and has nothing to
                switch to; the row never grows or loses a button meanwhile. */}
            <Show when={app.state.activeServer !== item.id}>
              <button onClick={() => app.run(() => app.switchServer(item.id))}>
                {t("Switch")}
              </button>
            </Show>
            <Show when={item.url}>
              <button
                onClick={() =>
                  app.run(async () => {
                    const input = await ask(t("Rename server"), [
                      {
                        name: "label",
                        label: t("Name"),
                        value: item.label,
                        required: true,
                      },
                    ]);
                    if (input)
                      app.set(
                        "servers",
                        await window.haarness.server({
                          id: item.id,
                          action: "rename",
                          label: input.label,
                        }),
                      );
                  })
                }
              >
                {t("Rename")}
              </button>
              <button
                onClick={() =>
                  app.run(async () => {
                    if (
                      await confirm(
                        t("Remove server {name}?", { name: item.label }),
                        t("This removes the saved connection and its projects from Desktop. Chat history and running tasks on the server are preserved."),
                        { submit: t("Remove server"), danger: true },
                      )
                    )
                      await app.removeServer(item.id);
                  })
                }
              >
                {t("Remove")}
              </button>
            </Show>
          </Row>
        )}
      </For>
      <div class="settings-after-list" data-setting="Add remote server">
        <button class="primary" onClick={openServerConnection}>
          {t("Add remote server")}
        </button>
      </div>
      <Computers />
    </>
  );
}
/** Your other computers, read from the account when this page opens.
 * Signed out, one quiet line is all there is: the only place that says the
 * computers can be opened here at all. */
function Computers() {
  const app = useApp();
  onCleanup(watchComputers());
  const active = () => app.currentServer().host;
  return (
    <Show
      when={computers.status !== "signed-out"}
      fallback={
        <div class="settings-computers-note settings-computers-signed-out" data-setting="Your computers">
          <p>{t("Sign in to Haarness to open your other computers here.")}</p>
          <button
            type="button"
            onClick={() => window.dispatchEvent(new CustomEvent("haarness:account"))}
          >
            {t("Sign in")}
          </button>
        </div>
      }
    >
    <section class="settings-computers" aria-label={t("Your computers")} data-setting="Your computers">
      <header>
        <div>
          <h3>{t("Your computers")}</h3>
          <p>
            {splitAround(
              t("Open a computer you share with {command}, through Haarness Cloud. Its prompts, tools and terminals run there."),
              () => <code>haarness remote enable</code>,
              "{command}",
            )}
          </p>
        </div>
        <Button
          icon="refresh"
          label={t("Refresh")}
          disabled={computers.status === "loading"}
          onClick={() => void loadComputers()}
        />
      </header>
      <Switch>
        <Match when={computers.status === "error"}>
          <div class="settings-computers-note">
            <p role="alert" title={computers.error}>
              {t("Couldn’t list your computers. Check your connection and try again.")}
            </p>
            <button onClick={() => void loadComputers()}>{t("Try again")}</button>
          </div>
        </Match>
        <Match when={!computers.items.length && computers.status !== "ready"}>
          <p class="settings-computers-note" role="status">
            {t("Looking for your computers…")}
          </p>
        </Match>
        <Match when={!computers.items.length}>
          <p class="settings-computers-note">
            {splitAround(
              t("No other computers yet. Run {command} on one to open it here."),
              () => <code>haarness remote enable</code>,
              "{command}",
            )}
          </p>
        </Match>
      </Switch>
      <For each={computers.items}>
        {(computer) => (
          <div
            class="setting-row computer-row"
            classList={{ away: !computer.connected }}
            data-setting={"server:host:" + computer.id}
          >
            <Icon name="host" size="md" />
            <div>
              <span class="computer-row-name">
                <strong>{computer.name}</strong>
                <Show when={active() === computer.id}>
                  <CurrentTag />
                </Show>
              </span>
              <p>
                {computer.connected
                  ? platformName(computer.platform) || t("Connected")
                  : lastSeen(computer)}
              </p>
            </div>
            <Show when={active() !== computer.id}>
              <button
                disabled={!computer.connected}
                onClick={() => app.run(() => app.openComputer(computer))}
              >
                {t("Switch")}
              </button>
            </Show>
          </div>
        )}
      </For>
    </section>
    </Show>
  );
}
function Projects() {
  const app = useApp();
  return (
    <>
      <Show when={app.state.connection}>
        {(connection) => (
          <section class="settings-current-project" aria-label={t("Current working folder")} data-setting="Current working folder">
            <div class="settings-current-project-icon"><Icon name="folder" size="lg" /></div>
            <div class="settings-current-project-details">
              <span class="small muted">{t("Current working folder")}</span>
              <h3>{connection().workspace.split(/[\\/]/).filter(Boolean).at(-1) || connection().workspace}</h3>
              <PathText class="settings-current-project-path" path={connection().workspace} local={!connection().remote} />
              <span class="small muted">{t(app.currentServer().label)}</span>
              <Show when={!connection().remote && !connection().trust}>
                <p class="small muted">{t("Project configuration disabled")}</p>
              </Show>
            </div>
            <Button icon="refresh" label={t("Refresh project")} onClick={() => app.run(app.hydrate)} />
          </section>
        )}
      </Show>
      <div class="setting-block">
        <h3>{t(app.currentServer().label)}</h3>
        <p>
          {t("Manage projects on this server. Switch servers to see another server’s projects.")}
        </p>
        <button
          class="primary"
          onClick={() => app.run(() => app.openProject())}
        >
          {t("Open project")}
        </button>
      </div>
      <Show when={!app.projects().length}>
        <Empty
          title={t("No projects yet")}
          detail={t("Open a project on this server to get started.")}
        />
      </Show>
      <For each={app.projects()}>
        {(item) => (
          <Row
            title={item.label}
            target={"project:" + item.id}
            detail={<PathText class="setting-row-path" path={item.root} local={!app.currentServer().url && !app.currentServer().host && !app.currentServer().cloud} />}
          >
            <button onClick={() => app.run(() => app.selectProject(item.id))}>
              {t("Open")}
            </button>
            {/* The sidebar's Remove project, in the same words; the name only
                adds to what the button says. */}
            <button
              aria-label={t("Remove project {name}", { name: item.label })}
              onClick={() =>
                app.run(async () => {
                  if (
                    await confirm(
                      t("Remove {name} from the sidebar?", { name: item.label }),
                      t(
                        "Chat history, running tasks and the saved server connection are preserved.",
                      ),
                      { submit: t("Remove project"), danger: true },
                    )
                  )
                    await app.forgetProject(item.id);
                })
              }
            >
              {t("Remove project")}
            </button>
          </Row>
        )}
      </For>

    </>
  );
}
type Cli = (args: string[]) => Promise<unknown>;
function Providers() {
  const app = useApp();
  const local = () => !app.state.connection?.remote;
  const [adding, setAdding] = createSignal(false);
  const [addingRouter, setAddingRouter] = createSignal(false);
  const [open, setOpen] = createSignal<Record<string, boolean>>({});
  const connected = () =>
    app.state.providers.filter((provider) => provider.authenticated);
  // The page lists what is connected; the catalog lives in Connect a
  // provider. Search still offers every provider, so one it opened that is
  // not connected shows here too, for the jump to land on.
  const sought = () => {
    const target = app.state.settingsTarget;
    return target?.startsWith("provider:")
      ? app.state.providers.find((provider) => !provider.authenticated && "provider:" + provider.id === target)
      : undefined;
  };
  const rows = () => {
    const extra = sought();
    return extra ? [...connected(), extra] : connected();
  };
  const count = (models: number) => (models === 1 ? t("1 model") : t("{count} models", { count: models }));
  let page!: HTMLDivElement;
  // The row goes once its provider is disconnected: the keyboard moves on to
  // the next row (or the one before), else to Connect a provider.
  async function disconnect(provider: (typeof app.state.providers)[number]) {
    const list = rows();
    const index = list.findIndex((item) => item.id === provider.id);
    const next = list[index + 1] ?? list[index - 1];
    await app.run(async () => {
      if (!(await disconnectProvider(app, provider))) return;
      queueMicrotask(() => {
        const row = next && page.parentElement?.querySelector<HTMLElement>(`[data-setting="${CSS.escape("provider:" + next.id)}"] [data-search-focus]`);
        (row ?? page.querySelector<HTMLElement>("button.primary"))?.focus();
      });
    });
  }
  return (
    <>
      <div ref={page} class="setting-block providers-hero" data-setting="Connect a provider">
        <header>
          <div>
            <h3>
              {connected().length
                ? t("Connected providers")
                : t("No provider connected yet")}
            </h3>
            <p class="muted">
              {connected().length
                ? app.state.connection?.remote
                  ? t("Sign in to another provider or add a key. Keys stay on this Haarness server.")
                  : t("Sign in to another provider or add a key. Keys stay on this computer.")
                : t("Sign in with a ChatGPT subscription, or paste an API key from OpenAI, Anthropic, Google and more.")}
            </p>
          </div>
          <button
            class="primary"
            onClick={() => app.openConnect({ onboarding: !connected().length })}
          >
            {t("Connect a provider")}
          </button>
        </header>
        <div class="toolbar">
          <button onClick={() => app.run(() => app.catalogs(true))}>
            {t("Refresh models")}
          </button>
          <Show when={local()}>
            <button onClick={() => setAdding(true)}>{t("Add custom provider")}</button>
            <button onClick={() => setAddingRouter(true)}>
              {t("Connect Haarness Router")}
            </button>
          </Show>
        </div>
      </div>
      <Show when={adding()}>
        <ProviderForm close={() => setAdding(false)} />
      </Show>
      <Show when={addingRouter()}>
        <RouterConnectionDialog close={() => setAddingRouter(false)} />
      </Show>
      <For each={rows()}>
        {(provider) => {
          const expanded = () => !!open()[provider.id];
          const list = "provider-models-" + provider.id.replace(/[^\w-]/g, "_");
          return (
            <div class="setting-block provider-row" data-setting={"provider:" + provider.id}>
              <header>
                <div>
                  <h3>
                    <ProviderLabel provider={provider.id} size="lg">
                      {provider.name}
                    </ProviderLabel>
                  </h3>
                  <p class="provider-status">
                    <span classList={{ added: provider.authenticated }}>
                      {provider.authenticated ? t("Connected") : t("Not connected")}
                    </span>
                    <Show when={provider.models.length}>
                      <span aria-hidden="true">·</span>
                      <button
                        type="button"
                        class="disclosure-toggle"
                        aria-expanded={expanded()}
                        aria-controls={list}
                        onClick={() => setOpen((state) => ({ ...state, [provider.id]: !expanded() }))}
                      >
                        {count(provider.models.length)}
                        <Icon name="chevron" size="xs" />
                      </button>
                    </Show>
                  </p>
                </div>
                <div class="toolbar">
                  {/* Haarness Cloud is the Haarness account, not a stored key:
                      "Disconnect" removed nothing and "Reconnect" opened a
                      picker that leaves Cloud out, so both go to the account. */}
                  <Show
                    when={provider.id !== "haarness"}
                    fallback={
                      <button data-search-focus onClick={() => window.dispatchEvent(new Event("haarness:account"))}>
                        {provider.authenticated ? t("Haarness account") : t("Sign in to Haarness")}
                      </button>
                    }
                  >
                    <button
                      data-search-focus
                      onClick={() => app.openConnect({ provider: provider.id, onboarding: false })}
                    >
                      {provider.authenticated ? t("Reconnect") : t("Connect")}
                    </button>
                  </Show>
                  <Show when={provider.authenticated && provider.id !== "haarness"}>
                    <button onClick={() => void disconnect(provider)}>
                      {t("Disconnect")}
                    </button>
                  </Show>
                </div>
              </header>
              <Show when={expanded()}>
                <ul class="provider-models" id={list}>
                  <For each={provider.models}>
                    {(model) => (
                      <li>
                        <strong>{modelName(provider.id, model.id, app.state.providers)}</strong>
                        <code>{model.id}</code>
                        <span>
                          {t("{count} context", { count: model.contextWindow != null ? formatNumber(model.contextWindow) : t("Unknown") })}
                        </span>
                      </li>
                    )}
                  </For>
                </ul>
              </Show>
            </div>
          );
        }}
      </For>
    </>
  );
}
function Models() {
  const app = useApp();
  const [filter, setFilter] = createSignal("");
  const [all, setAll] = createSignal(false);
  // Search finds every model; one of a provider that is not connected opens
  // the whole catalog so the jump lands on it.
  createEffect(() => {
    const target = app.state.settingsTarget;
    if (!target?.startsWith("model:")) return;
    const id = target.slice("model:".length);
    if (app.state.providers.some((provider) => !provider.authenticated && id.startsWith(provider.id + ":")))
      setAll(true);
  });
  const matches = (provider: T.ProviderInfo, model: T.ProviderInfo["models"][number]) =>
    (modelName(provider.id, model.id, app.state.providers) + " " + model.id + " " + provider.name)
      .toLowerCase()
      .includes(filter().trim().toLowerCase());
  const rows = () =>
    app.state.providers
      .filter((provider) => all() || provider.authenticated)
      .flatMap((provider) => provider.models.map((model) => ({ provider, model })))
      .filter(({ provider, model }) => matches(provider, model));
  // While searching, say how many more the other providers have.
  const elsewhere = () =>
    all() || !filter().trim()
      ? 0
      : app.state.providers
          .filter((provider) => !provider.authenticated)
          .reduce((sum, provider) => sum + provider.models.filter((model) => matches(provider, model)).length, 0);
  const anyConnected = () => app.state.providers.some((provider) => provider.authenticated);
  return (
    <>
      <div class="toolbar models-toolbar">
        <input
          placeholder={t("Search models…")}
          aria-label={t("Search models")}
          value={filter()}
          onInput={(event) => setFilter(event.currentTarget.value)}
        />
        <label class="quiet-switch">
          <input
            type="checkbox"
            role="switch"
            aria-description={t("Include models of providers that aren't connected")}
            checked={all()}
            onChange={(event) => setAll(event.currentTarget.checked)}
          />
          <span>{t("Show all")}</span>
        </label>
      </div>
      <Show
        when={rows().length}
        fallback={
          <Show
            when={filter().trim() || all() || anyConnected()}
            fallback={
              <Empty
                title={t("No provider connected yet")}
                detail={t("Sign in with a ChatGPT subscription, or paste an API key from OpenAI, Anthropic, Google and more.")}
              />
            }
          >
            <p class="muted">{t("No model matches that search.")}</p>
          </Show>
        }
      >
        <table>
          <thead>
            <tr>
              <th>{t("Model")}</th>
              <th>{t("Provider")}</th>
              <th>{t("Context")}</th>
              <th>{t("Capabilities")}</th>
            </tr>
          </thead>
          <tbody>
            <For each={rows()}>
              {({ provider, model }) => (
                <tr data-setting={"model:" + provider.id + ":" + model.id}>
                  <td>
                    {modelName(provider.id, model.id, app.state.providers)}
                    <small>{model.id}</small>
                  </td>
                  <td>
                    <ProviderLabel provider={provider.id}>
                      {provider.name}
                    </ProviderLabel>
                  </td>
                  <td>{model.contextWindow != null ? formatNumber(model.contextWindow) : "—"}</td>
                  <td>
                    {[
                      model.tools ? t("Tools") : "",
                      model.images ? t("Images") : "",
                      model.video ? t("Video") : "",
                      model.reasoning ? t("Reasoning") : "",
                    ]
                      .filter(Boolean)
                      .join(" · ")}
                  </td>
                </tr>
              )}
            </For>
          </tbody>
        </table>
      </Show>
      <Show when={elsewhere()}>
        <p class="models-elsewhere">
          {elsewhere() === 1
            ? t("1 more from providers that aren't connected.")
            : t("{count} more from providers that aren't connected.", { count: elsewhere() })}{" "}
          <button type="button" class="settings-link" onClick={() => setAll(true)}>
            {t("Show all")}
          </button>
        </p>
      </Show>
    </>
  );
}
// English keys; t() runs where they render so the language can change live.
const assetCopy = {
  agent: {
    about: "Agents are assistants with their own instructions, model and tools. Switch between them in the composer.",
    create: "Create agent",
    empty: "No agents yet",
    emptyDetail: "Create one for a role you repeat, like a reviewer or a release helper.",
    removeProject: "Remove this custom agent from this project?",
    removeAll: "Remove this custom agent from all projects?",
  },
  command: {
    about: "Commands are saved prompts you run with / in any chat.",
    create: "Create command",
    empty: "No commands yet",
    emptyDetail: "Save a prompt you use often, then run it with /name.",
    removeProject: "Remove this custom command from this project?",
    removeAll: "Remove this custom command from all projects?",
  },
  skill: {
    about: "Skills are instructions and files an agent loads when a task calls for them.",
    create: "Create skill",
    empty: "No skills yet",
    emptyDetail: "Teach Haarness a workflow once, and it will use it whenever it fits.",
    removeProject: "Remove this custom skill from this project?",
    removeAll: "Remove this custom skill from all projects?",
  },
} as const;
function Assets(props: { kind: string; cli: Cli }) {
  const app = useApp();
  const [rows, setRows] = createSignal<(AssetRow & DefinitionRow)[]>([]);
  const [plugins, setPlugins] = createSignal<T.PluginInfo[]>([]);
  const kind = () =>
    props.kind === "agents"
      ? "agent"
      : props.kind === "skills"
        ? "skill"
        : "command";
  // Agents and skills can be switched off without deleting them. The server
  // keeps that state apart from the definitions, so read it alongside them.
  const [enabled, setEnabled] = createSignal<Record<string, boolean>>({});
  let latest = 0;
  async function refresh() {
    const request = ++latest;
    const [list, state, installed] = await Promise.all([
      app.native<(AssetRow & DefinitionRow)[]>({
        action: "asset",
        kind: kind(),
        input: { action: "list" },
      }),
      kind() === "command"
        ? undefined
        : app
            .api<Record<string, { name: string; enabled?: boolean }[]>>(
              "manageResource",
              "capabilities",
              { action: "list" },
            )
            .catch(() => undefined),
      // Plugins name where a definition that is not the person's own comes from.
      app.api<T.PluginInfo[]>("plugins").catch(() => [] as T.PluginInfo[]),
    ]);
    // The pane serves agents, commands and skills in turn: a slower answer
    // for the section shown before must not replace this one.
    if (request !== latest) return;
    setRows(list);
    setPlugins(installed);
    setEnabled(
      Object.fromEntries(
        (state?.[props.kind] ?? []).map((row) => [row.name, row.enabled !== false]),
      ),
    );
  }
  async function toggle(item: AssetRow, on: boolean) {
    await app.api("manageResource", "capabilities", {
      action: on ? "enable" : "disable",
      kind: kind(),
      name: item.name,
      // A base folder item is shared by every project, as a global one is.
      scope: item.scope === "global" || item.scope === "base" ? "global" : "local",
    });
    await app.catalogs();
    await refresh();
  }
  createEffect(() => {
    props.kind;
    app.state.catalogVersion;
    void app.run(refresh);
  });
  const [editing, setEditing] = createSignal<{
    prior: Record<string, unknown>;
    editing: boolean;
  }>();
  const sourceLabel = (item: DefinitionRow) => {
    const source = definitionSource(item, plugins());
    return source === "builtin" ? t("Built-in") : source;
  };
  async function edit(item?: AssetRow) {
    let prior: Record<string, unknown> = {};
    if (item)
      prior = await app.native({
        action: "asset",
        kind: kind(),
        input: {
          action: "get",
          name: item.name,
          ...(item.scope ? { scope: item.scope } : {}),
        },
      });
    setEditing({ prior, editing: !!item });
  }
  return (
    <>
      <p class="muted">
        {t(assetCopy[kind()].about)}{" "}
        {t("Changes to built-in or plugin definitions are saved as your own version.")}
      </p>
      <button class="primary" onClick={() => app.run(() => edit())}>
        {t(assetCopy[kind()].create)}
      </button>
      <Show when={editing()} keyed>
        {(current) => (
          <AssetForm
            kind={kind()}
            prior={current.prior}
            editing={current.editing}
            close={() => setEditing(undefined)}
            saved={() => {
              void app.run(async () => {
                await app.catalogs();
                await refresh();
              });
            }}
          />
        )}
      </Show>
      <Show when={!rows().length && !editing()}>
        <Empty title={t(assetCopy[kind()].empty)} detail={t(assetCopy[kind()].emptyDetail)} />
      </Show>
      <For each={rows()}>
        {(item) => (
          <Row
            title={item.name}
            description={[
              item.description,
              item.scope === "local"
                ? t("This project")
                : item.scope === "global"
                  ? t("All projects")
                  : sourceLabel(item),
            ]
              .filter(Boolean)
              .join(" · ")}
            tag={item.scope === "base" ? <BaseTag /> : undefined}
            target={props.kind + ":" + item.name}
          >
            <Show when={item.name in enabled()}>
              <input
                type="checkbox"
                role="switch"
                aria-label={
                  kind() === "agent"
                    ? t("Enable agent {name}", { name: item.name })
                    : t("Enable skill {name}", { name: item.name })
                }
                checked={enabled()[item.name]}
                onChange={(event) => {
                  const on = event.currentTarget.checked;
                  void app.run(() => toggle(item, on));
                }}
              />
            </Show>
            <button onClick={() => app.run(() => edit(item))}>{t("Edit")}</button>
            <Show when={item.managed}>
              <button
                onClick={() =>
                  app.run(async () => {
                    if (
                      await confirm(
                        t(item.customized ? "Reset {name}" : "Remove {name}", { name: item.name }),
                        t(item.scope === "local" ? assetCopy[kind()].removeProject : assetCopy[kind()].removeAll),
                        { submit: item.customized ? t("Reset") : t("Remove"), danger: true },
                      )
                    ) {
                      await app.native({
                        action: "asset",
                        kind: kind(),
                        input: {
                          action: "delete",
                          name: item.name,
                          scope: item.scope,
                        },
                      });
                      await refresh();
                    }
                  })
                }
              >
                {item.customized ? t("Reset to default") : t("Remove")}
              </button>
            </Show>
          </Row>
        )}
      </For>
    </>
  );
}
function Tools() {
  const app = useApp();
  const [catalog, setCatalog] = createSignal<T.ToolCatalogResponse>(),
    [filter, setFilter] = createSignal(""),
    // Groups the reader opened or closed; the rest follow their size. A
    // search opens every group it finds something in, until closed there.
    [opened, setOpened] = createSignal<Record<string, boolean>>({}),
    [closedInSearch, setClosedInSearch] = createSignal<Record<string, boolean>>({});
  async function refresh() {
    setCatalog(
      await app.api(
        "toolCatalog",
        app.state.selected ? { threadId: app.state.selected } : {},
      ),
    );
  }
  createEffect(() => {
    app.state.catalogVersion;
    void app.run(refresh);
  });
  // A tool Search opened is shown in its group, which opens for it.
  createEffect(() => {
    const target = app.state.settingsTarget;
    const tool = target?.startsWith("tool:")
      ? catalog()?.tools.find((item) => "tool:" + item.id === target)
      : undefined;
    if (tool) setOpened((state) => ({ ...state, [toolGroupKey(tool)]: true }));
  });
  const needle = () => filter().trim().toLowerCase();
  const groups = () =>
    toolGroups(
      (catalog()?.tools ?? []).filter(
        (tool) =>
          !needle() ||
          `${tool.id} ${toolTitle(tool)} ${tool.description}`.toLowerCase().includes(needle()),
      ),
    );
  const expanded = (group: ReturnType<typeof groups>[number]) =>
    needle()
      ? !closedInSearch()[group.key]
      : (opened()[group.key] ?? group.tools.length <= OPEN_GROUP_LIMIT);
  const toggle = (group: ReturnType<typeof groups>[number]) => {
    const open = !expanded(group);
    if (needle()) setClosedInSearch((state) => ({ ...state, [group.key]: !open }));
    else setOpened((state) => ({ ...state, [group.key]: open }));
  };
  const groupTitle = (group: ReturnType<typeof groups>[number]) =>
    group.kind === "builtin" ? t("Built-in") : group.name;
  const groupMeta = (group: ReturnType<typeof groups>[number]) => {
    const off = group.tools.filter((tool) => !tool.enabled).length;
    return [
      group.kind === "mcp" ? t("MCP server") : group.kind === "extension" ? t("Extension") : "",
      group.tools.length === 1 ? t("1 tool") : t("{count} tools", { count: group.tools.length }),
      off ? t("{count} turned off", { count: off }) : "",
    ]
      .filter(Boolean)
      .join(" · ");
  };
  return (
    <>
      <Row
        title={t("Tool loading")}
        description={t("Control how optional tool schemas enter model context.")}
      >
        <Select
          aria-label={t("Tool loading")}
          value={catalog()?.lazyMode ?? "auto"}
          onChange={(value) =>
            app.run(async () =>
              setCatalog(await app.api("updateLazyTools", { mode: value })),
            )
          }
          options={[
            { value: "auto", label: t("Automatic") },
            { value: "always", label: t("Always lazy") },
            { value: "off", label: t("All enabled tools") },
          ]}
        />
      </Row>
      <div class="toolbar tools-toolbar">
        <input
          aria-label={t("Search tools")}
          placeholder={t("Find a tool…")}
          onInput={(event) => {
            setFilter(event.currentTarget.value);
            setClosedInSearch({});
          }}
        />
        <Show when={catalog()}>
          {(current) => (
            <span class="tools-tokens">
              {t("{advertised} advertised tokens · {saved} saved", {
                advertised: formatNumber(current().estimatedAdvertisedTokens),
                saved: formatNumber(current().estimatedSavedTokens),
              })}
            </span>
          )}
        </Show>
      </div>
      <Show when={catalog() && needle() && !groups().length}>
        <p class="muted">{t("No tool matches that search.")}</p>
      </Show>
      <For each={groups()}>
        {(group) => {
          const body = "tool-group-" + group.key.replace(/[^\w-]/g, "_");
          return (
            <div class="tool-group">
              <button
                type="button"
                class="tool-group-toggle disclosure-toggle"
                aria-expanded={expanded(group)}
                aria-controls={body}
                onClick={() => toggle(group)}
              >
                <Icon name="chevron" size="xs" />
                <strong>{groupTitle(group)}</strong>
                <span>{groupMeta(group)}</span>
              </button>
              <Show when={expanded(group)}>
                <div id={body}>
                  <For each={group.tools}>
                    {(tool) => (
                      <Row
                        literal
                        title={toolTitle(tool)}
                        description={toolSummary(tool.description)}
                        tag={<code class="setting-id">{tool.id}</code>}
                        target={"tool:" + tool.id}
                      >
                        <input
                          type="checkbox"
                          role="switch"
                          aria-label={t("Enable {name}", { name: tool.id })}
                          checked={tool.enabled}
                          onChange={(event) =>
                            app.run(async () =>
                              setCatalog(
                                await app.api("updateTool", tool.id, {
                                  enabled: event.currentTarget.checked,
                                }),
                              ),
                            )
                          }
                        />
                      </Row>
                    )}
                  </For>
                </div>
              </Show>
            </div>
          );
        }}
      </For>
    </>
  );
}
function Mcp(props: { cli: Cli }) {
  const app = useApp();
  const [rows, setRows] = createSignal<T.McpServerInfo[]>([]);
  const connection = () => app.state.connection?.id ?? "";
  async function refresh() {
    const servers = await app.api<T.McpServerInfo[]>("mcpServers");
    setRows(servers);
    noteMcpServers(connection(), servers);
    if (!app.state.connection?.remote)
      await loadReadOnlyServers(
        connection(),
        servers.map((server) => server.name),
      ).catch(() => undefined);
  }
  // A server of the base folder, which Haarness lists but never changes.
  const fromBase = (server: T.McpServerInfo) => server.scope === "base";
  createEffect(() => {
    app.state.catalogVersion;
    void app.run(refresh);
  });
  const [adding, setAdding] = createSignal(false);
  return (
    <>
      <Show when={!app.state.connection?.remote}>
        <button class="primary" onClick={() => setAdding(true)}>
          {t("Add MCP server")}
        </button>
      </Show>
      <Show when={adding()}>
        <McpForm
          close={() => setAdding(false)}
          platform={app.state.platform}
          storeSecret={async (name, secret) =>
            (await app.native<{ name: string }>({ action: "store_mcp_secret", name, secret })).name
          }
          removeSecrets={(names) => app.native({ action: "remove_mcp_secrets", names })}
          save={async (input) => {
            await app.native({ action: "asset", kind: "mcp", input });
            // Asset events refresh the live registry once the daemon has reloaded.
            void app.run(async () => {
              await app.catalogs();
              await refresh();
            });
          }}
        />
      </Show>
      <For each={rows()}>
        {(server) => (
          <div class="setting-block">
            <header>
              <h3 data-setting={"mcp:" + server.name}>{server.name}</h3>
              <Show when={fromBase(server)}>
                <BaseTag />
              </Show>
              <span class="badge">{server.status}</span>
            </header>
            {/* Why a server failed is what decides the next step (reconnect, fix a setting). */}
            <Show when={server.status === "failed" && server.detail}>
              <p class="mcp-server-detail">{server.detail}</p>
            </Show>
            <div class="toolbar">
              <button
                onClick={() =>
                  app.run(async () => {
                    await app.api("reconnectMcp", server.name);
                    await refresh();
                  })
                }
              >
                {t("Reconnect")}
              </button>
              <button
                onClick={() =>
                  app.run(async () => {
                    await app.api("disconnectMcp", server.name);
                    await refresh();
                  })
                }
              >
                {t("Disconnect")}
              </button>
              <Show when={!app.state.connection?.remote}>
                <button
                  onClick={() =>
                    app.run(() => props.cli(["mcp", "auth", server.name]))
                  }
                >
                  {t("Authorize")}
                </button>
                <MenuButton
                  text={t("Manage")}
                  label={t("Manage {name}", { name: server.name })}
                  items={[
                    { label: "Enable", command: "enable" },
                    { label: "Disable", command: "disable" },
                    // The base folder's servers are turned off, not removed.
                    ...(fromBase(server) ? [] : [{ label: "Remove", command: "remove" }]),
                    { label: "Sign out", command: "logout" },
                  ].map((item) => ({
                    label: t(item.label),
                    run: () =>
                      void app.run(async () => {
                        const input = await ask(
                          `${t(item.label)} ${server.name}`,
                          [
                            {
                              // The options are CLI scopes; the select shows them as they are.
                              name: "scope",
                              label: t("Scope"),
                              type: "select",
                              options: ["Global", "Project"],
                            },
                          ],
                          { submit: t(item.label) },
                        );
                        if (!input) return;
                        await props.cli([
                          "mcp",
                          item.command,
                          server.name,
                          ...(input.scope === "Project" ? ["--project"] : []),
                        ]);
                        await refresh();
                      }),
                  }))}
                />
                <Show when={offersApps(connection(), server)}>
                  {/* Pages this server shows under its tools; off, it is a plain MCP server. */}
                  <label
                    class="mcp-apps-switch"
                    classList={{ locked: readOnlyServer(connection(), server.name) }}
                    title={
                      readOnlyServer(connection(), server.name)
                        ? t("Set outside your settings")
                        : t("Show this server's pages under its tools in chats. Off, its tools show as text.")
                    }
                  >
                    <input
                      type="checkbox"
                      role="switch"
                      aria-label={t("Interactive apps for {name}", { name: server.name })}
                      disabled={readOnlyServer(connection(), server.name)}
                      checked={!appsOff(connection(), server.name)}
                      onChange={(event) => {
                        const on = event.currentTarget.checked;
                        void app.run(() => setApps(connection(), server.name, on));
                      }}
                    />
                    <span>{t("Interactive apps")}</span>
                  </label>
                </Show>
              </Show>
            </div>
          </div>
        )}
      </For>
      <Show when={!rows().length}>
        <Empty
          title={t("Connect your tools.")}
          detail={t("MCP servers bring external tools, resources and prompts into Haarness.")}
        />
      </Show>
    </>
  );
}
function Plugins() {
  const app = useApp();
  const [rows, setRows] = createSignal<T.PluginInfo[]>([]);
  async function refresh() {
    setRows(await app.api("plugins"));
  }
  createEffect(() => {
    app.state.catalogVersion;
    void app.run(refresh);
  });
  const [installing, setInstalling] = createSignal<"plugin" | "marketplace">();
  async function action(action: string, target: string) {
    await app.api("pluginAction", { action, target });
    await refresh();
    await app.catalogs();
  }
  return (
    <>
      <p class="muted">
        {t("Install compatible plugin bundles with skills, commands, agents, MCP servers and extensions.")}
      </p>
      <div class="toolbar">
        <button class="primary" onClick={() => setInstalling("plugin")}>
          {t("Install plugin")}
        </button>
        <button onClick={() => setInstalling("marketplace")}>
          {t("Register marketplace")}
        </button>
      </div>
      <Show when={installing()} keyed>
        {(kind) => (
          <SourceForm
            kind={kind}
            close={() => setInstalling(undefined)}
            save={(target) =>
              action(kind === "plugin" ? "install" : "marketplace", target)
            }
          />
        )}
      </Show>
      <Show when={!rows().length && !installing()}>
        <Empty
          title={t("No plugins installed")}
          detail={t("Plugins bundle skills, commands, agents, MCP servers and extensions. Install one from a folder, a Git repository or a marketplace.")}
        />
      </Show>
      <For each={rows()}>
        {(plugin) => (
          <Row
            title={plugin.name}
            target={"plugin:" + plugin.name}
            description={plugin.description || plugin.source}
            tag={plugin.source === "haarness-base" ? <BaseTag /> : undefined}
          >
            <Show when={plugin.managed}>
              <input
                type="checkbox"
                role="switch"
                aria-label={t("Enable plugin {name}", { name: plugin.name })}
                checked={plugin.enabled}
                onChange={(event) =>
                  app.run(() =>
                    action(
                      event.currentTarget.checked ? "enable" : "disable",
                      plugin.name,
                    ),
                  )
                }
              />
              <Show when={plugin.source === "haarness"}>
                <button
                  onClick={() => app.run(() => action("update", plugin.name))}
                >
                  {t("Update")}
                </button>
              </Show>
              <button
                onClick={() =>
                  app.run(async () => {
                    if (await confirm(t("Remove plugin?"), plugin.name, { submit: t("Remove plugin"), danger: true }))
                      await action("remove", plugin.name);
                  })
                }
              >
                {t("Remove")}
              </button>
            </Show>
          </Row>
        )}
      </For>
    </>
  );
}
interface ExtensionConfig {
  enabled: boolean;
  timeout_ms: number;
  entries: {
    name?: string;
    source: string;
    enabled: boolean;
    options: unknown;
  }[];
}
function Extensions(props: { cli: Cli }) {
  const app = useApp();
  const [scope, setScope] = createSignal(app.state.settingsTarget?.startsWith("extension:project:") ? "Project" : "Global");
  createEffect(() => {
    const target = app.state.settingsTarget;
    if (target?.startsWith("extension:")) setScope(target.startsWith("extension:project:") ? "Project" : "Global");
  });
  const flags = () => (scope() === "Project" ? ["--project"] : []);
  const [config, setConfig] = createSignal<ExtensionConfig>();
  async function refresh() {
    setConfig(
      await app.native({
        action: "extensions",
        project: scope() === "Project",
      }),
    );
  }
  createEffect(() => {
    void app.run(refresh);
  });
  const [installing, setInstalling] = createSignal(false);
  async function install(source: string) {
    await props.cli([
      "extension",
      ...flags(),
      "install",
      source,
      "--options",
      JSON.stringify(
        config()?.entries.find((entry) => entry.source === source)?.options ??
          {},
      ),
    ]);
    await refresh();
  }
  return (
    <>
      <Show when={installing()}>
        <SourceForm
          kind="extension"
          close={() => setInstalling(false)}
          save={install}
        />
      </Show>
      <div class="toolbar" data-setting="Extension scope">
        <label>
          {t("Scope")}{" "}
          <Select
            aria-label={t("Extension scope")}
            value={scope()}
            onChange={(value) => setScope(value)}
            options={[
              { value: "Global", label: t("Global") },
              { value: "Project", label: t("Project") },
            ]}
          />
        </label>
      </div>
      <Row
        title={t("Enable extensions")}
        description={
          scope() === "Project"
            ? t("Extensions configured for this project.")
            : t("Extensions available across your projects.")
        }
      >
        <input
          role="switch"
          type="checkbox"
          aria-label={t("Enable extensions")}
          checked={config()?.enabled ?? false}
          onChange={(event) =>
            app.run(async () => {
              await props.cli([
                "extension",
                ...flags(),
                event.currentTarget.checked ? "enable" : "disable",
              ]);
              await refresh();
            })
          }
        />
      </Row>
      <div class="toolbar" data-setting="Install extension">
        <button class="primary" onClick={() => setInstalling(true)}>
          {t("Install extension")}
        </button>
      </div>
      <For each={config()?.entries}>
        {(entry, index) => (
          <div class="setting-block" data-setting={"extension:" + scope().toLowerCase() + ":" + entry.source}>
            <header>
              <h3>{entry.name ?? "Extension " + (index() + 1)}</h3>
              <input
                type="checkbox"
                role="switch"
                aria-label={t("Enable extension {name}", { name: entry.name ?? index() + 1 })}
                checked={entry.enabled}
                onChange={(event) =>
                  app.run(async () => {
                    await props.cli([
                      "extension",
                      ...flags(),
                      event.currentTarget.checked ? "enable" : "disable",
                      String(index() + 1),
                    ]);
                    await refresh();
                  })
                }
              />
            </header>
            <p class="path">{entry.source}</p>
            <div class="toolbar">
              <button
                onClick={() =>
                  app.run(async () => {
                    if (
                      await confirm(
                        t("Remove extension?"),
                        t("Unregister {name}. Source files are preserved.", { name: entry.name ?? entry.source }),
                        { submit: t("Remove extension"), danger: true },
                      )
                    ) {
                      await props.cli([
                        "extension",
                        ...flags(),
                        "remove",
                        String(index() + 1),
                      ]);
                      await refresh();
                    }
                  })
                }
              >
                {t("Remove")}
              </button>
            </div>
          </div>
        )}
      </For>
      <p class="muted">{t("Changes take effect after the current task finishes.")}</p>
    </>
  );
}
function Profiles(props: { cli: Cli }) {
  const app = useApp();
  /**
   * Restore from a file (`restore <path>`) or the catalog (`cloud-restore
   * <id> --url …`): preview it, list the files it would replace, and replace
   * them only once the reader confirms.
   */
  async function restore(source: string[]) {
    const plan = restorePlan(
      await props.cli(["profile", ...source, "--dry-run", "--json"]),
    );
    const summary = restoreSummary(plan);
    if (
      !(await confirm(t("Restore profile?"), summary.description, {
        submit: t("Restore profile"),
        danger: summary.replaces,
        list: summary.list,
      }))
    )
      return;
    await props.cli(restoreArgs(source, plan));
    app.notify(restoredMessage(plan));
  }
  return (
    <>
      <p class="muted">
        {t("Portable profiles contain configuration, skills, agents, commands, and other supported assets. They exclude chat databases and credential stores.")}
      </p>
      <Row title="Save a profile">
        <button
          onClick={() =>
            app.run(async () => {
              const input = await ask(t("Save profile"), [
                { name: "name", label: t("Name"), required: true },
                {
                  name: "scope",
                  label: t("Scope"),
                  type: "select",
                  options: ["both", "global", "local"],
                },
              ]);
              if (input)
                await props.cli([
                  "profile",
                  "save",
                  input.name,
                  "--scope",
                  input.scope,
                  "--json",
                ]);
            })
          }
        >
          {t("Save profile")}
        </button>
      </Row>
      <Row
        title="Restore configuration"
        description="Preview the changes before confirming restoration."
      >
        <button
          onClick={() =>
            app.run(async () => {
              const path = await window.haarness.pick("file");
              if (path) await restore(["restore", path]);
            })
          }
        >
          {t("Restore profile")}
        </button>
      </Row>
      <Row title="Profile catalog">
        <MenuButton
          text={t("Manage catalog")}
          label={t("Manage catalog")}
          items={[
            { label: "List profiles", command: "cloud-list" },
            { label: "Upload profile", command: "cloud-push" },
            { label: "View profile", command: "cloud-get" },
            { label: "Download profile", command: "cloud-download" },
            { label: "Restore profile", command: "cloud-restore" },
            { label: "Delete profile", command: "cloud-delete" },
          ].map((item) => ({
            label: t(item.label),
            run: () =>
              void app.run(async () => {
                // Every catalog action needs the same server, so offer the last one.
                const last = await window.haarness.storage("profile-catalog:v1");
                const input = await ask(
                  t(item.label),
                  [
                    {
                      name: "url",
                      label: t("Catalog server URL"),
                      value: typeof last === "string" ? last : "",
                      placeholder: "https://haarness.example.com",
                      required: true,
                    },
                    ...(item.command === "cloud-list"
                      ? []
                      : [
                          {
                            name: "target",
                            label:
                              item.command === "cloud-push"
                                ? t("Local archive path")
                                : t("Profile ID"),
                            required: true,
                          },
                        ]),
                    ...(item.command === "cloud-download"
                      ? [
                          {
                            name: "output",
                            label: t("Download output path"),
                            required: true,
                          },
                        ]
                      : []),
                  ],
                  {
                    description: t("Profiles are shared through the catalog of a Haarness server you run. Enter its address; Haarness signs in with the token in HAARNESS_PROFILE_TOKEN."),
                    submit: t(item.label),
                    danger: item.command === "cloud-delete",
                  },
                );
                if (!input) return;
                await window.haarness.storage("profile-catalog:v1", input.url.trim());
                if (item.command === "cloud-restore")
                  return restore(["cloud-restore", input.target, "--url", input.url]);
                const args = [
                  "profile",
                  item.command,
                  ...(input.target ? [input.target] : []),
                  "--url",
                  input.url,
                  "--json",
                ];
                if (input.output) args.push("--output", input.output);
                await props.cli(args);
              }),
          }))}
        />
      </Row>
    </>
  );
}
/**
 * A link in release notes opens in the system browser when it is a web
 * address; anything else (a relative link, a file, an app link) does nothing:
 * the notes come from the update feed, not from a project.
 */
function openReleaseLink(href: string) {
  let url: URL;
  try {
    url = new URL(href);
  } catch {
    return;
  }
  if (url.protocol === "https:" || url.protocol === "http:")
    void window.haarness.openExternal(url.href).catch(() => {});
}
/** An update's release notes, rendered and sanitized, in a scroll area of their own. */
function ReleaseNotes(props: { version?: string; notes: string }) {
  return (
    <section class="release-notes" aria-label={t("Release notes")}>
      <small>
        {props.version ? t("What’s new in {version}", { version: props.version }) : t("What’s new")}
      </small>
      <div class="release-notes-body" tabindex="0">
        <Markdown text={props.notes} document onLink={openReleaseLink} />
      </div>
    </section>
  );
}
function About() {
  const app = useApp();
  const [update, setUpdate] = createSignal<{
    status?: string;
    percent?: number;
    version?: string;
    message?: string;
    /** The feed's release notes for the update, as Markdown; "" without any. */
    notes?: string;
  }>({});
  const off = window.haarness.onEvent((event) => {
    if (event.kind !== "update") return;
    const next = event.data as ReturnType<typeof update>;
    // Progress events carry no version or notes; the ones the check found stay.
    setUpdate((current) => ({
      ...next,
      version: next.version ?? current.version,
      notes: next.notes || current.notes,
    }));
  });
  onCleanup(off);
  return (
    <>
      <Wordmark class="about-mark" />
      <p>Haarness {app.state.version}</p>
      <p class="muted">{t("A place for your projects and chats.")}</p>
      <Row title="Updates">
        <button
          onClick={() =>
            app.run(async () => {
              const result = (await window.haarness.update("check")) as {
                available: boolean;
                info?: { version: string };
                notes?: string;
              };
              setUpdate({
                status: result.available ? "available" : "current",
                version: result.info?.version,
                notes: result.notes,
              });
            })
          }
        >
          {t("Check for updates")}
        </button>
        <Show when={update().status === "available"}>
          <button
            onClick={() => app.run(() => window.haarness.update("download"))}
          >
            {t("Download {version}", { version: update().version ?? "" })}
          </button>
        </Show>
        <Show when={update().status === "ready"}>
          <button
            class="primary"
            onClick={() =>
              app.run(async () => {
                const { version, notes } = update();
                if (
                  await confirm(
                    t("Install update?"),
                    t("Haarness will restart. Save your file edits before continuing."),
                    {
                      submit: t("Restart & install"),
                      // What the update brings, when the feed says.
                      content: notes
                        ? () => <ReleaseNotes version={version} notes={notes} />
                        : undefined,
                    },
                  )
                )
                  await window.haarness.update("install");
              })
            }
          >
            {t("Restart & install")}
          </button>
        </Show>
        <Show when={update().status === "downloading"}>
          <span>{t("Downloading {percent}%", { percent: Math.round(update().percent ?? 0) })}</span>
        </Show>
        <Show when={update().status === "current"}>
          <span>{t("You are up to date.")}</span>
        </Show>
        <Show when={update().message}>
          <span class="danger">{update().message}</span>
        </Show>
      </Row>
      <Show when={app.state.connection && !app.state.connection.remote}>
        <Row
          title="Reopen project"
          description="Reload the current project to bring chats and files up to date."
        >
          <button onClick={() => app.run(app.hydrate)}>{t("Refresh")}</button>
        </Row>
      </Show>
    </>
  );
}

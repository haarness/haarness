import { formatDateTime, formatNumber, t, errorLine } from "./i18n";
import {
  For,
  Show,
  createEffect,
  createMemo,
  createSignal,
  onCleanup,
  untrack,
} from "solid-js";
import { useApp } from "./state";
import { Select } from "./Select";
import { SettingRow as Row } from "./SettingRow";
import { Icon, confirm } from "./ui";
import { cancelled } from "./error-text";
import { ChromeSettingsSection } from "./ChromeSettings";
import type { ChromeSettings, ChromeSettingsView } from "../chrome";
import type {
  BrowserAccess,
  BrowserData,
  BrowserDecision,
  BrowserPolicy,
  BrowserSettingsValue,
} from "../browser-settings";
import "./browser-settings.css";
/** A server with Chrome support adds Chrome's own (global) settings. */
type SettingsValue = BrowserSettingsValue & { chrome?: ChromeSettingsView };
const decisions = [
  { value: "allow", label: "Always allow" },
  { value: "ask", label: "Requires approval" },
  { value: "deny", label: "Block" },
];
export function BrowserSettings() {
  const app = useApp();
  const [scope, setScope] = createSignal("project");
  const projectConfig = () => app.state.connection?.info.projectTrusted !== false;
  createEffect(() => { if (!projectConfig()) setScope("global"); });
  const [value, setValue] = createSignal<SettingsValue>();
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal("");
  const [origin, setOrigin] = createSignal("");
  const [records, setRecords] = createSignal<BrowserData>({
    history: [],
    downloads: [],
  });
  const [manager, setManager] = createSignal<"history" | "downloads">();
  const [query, setQuery] = createSignal("");
  const [dataBusy, setDataBusy] = createSignal(false);
  const connectionID = createMemo(() => app.state.connection?.id);
  const supported = createMemo(() =>
    app.state.connection?.info.capabilities.includes("browser-settings.v1"),
  );
  const policy = () => value()?.scopePolicy ?? value()?.inheritedPolicy;
  let generation = 0;
  let refreshInFlight = false;
  async function request(
    input: Record<string, unknown> = { action: "get" },
    reset = false,
    background = false,
  ) {
    const connection = app.state.connection?.id;
    if (!connection || !supported() || (busy() && !reset) || (background && refreshInFlight)) return;
    const version = ++generation;
    // Polling must not interrupt typing or steal a click by disabling controls.
    // A user mutation advances generation, so an older refresh cannot replace it.
    if (background) refreshInFlight = true;
    else setBusy(true);
    if (reset) setValue(undefined);
    try {
      const result = await window.haarness.api<{ data: SettingsValue }>(
        connection,
        "manageResource",
        ["browser-settings", { ...input, scope: scope() }],
      );
      if (version !== generation || connection !== app.state.connection?.id)
        return;
      // Preserve row/control identity during background refreshes.
      if (
        value()?.revision !== result.data.revision ||
        value()?.scope !== result.data.scope
      )
        setValue(result.data);
      setError("");
      if (input.action === "set") setOrigin("");
    } catch (cause) {
      if (version === generation && !cancelled(cause)) {
        setError(errorLine(cause));
        const fresh = await window.haarness
          .api<{ data: BrowserSettingsValue }>(connection, "manageResource", [
            "browser-settings",
            { action: "get", scope: scope() },
          ])
          .catch(() => undefined);
        if (version === generation && fresh) setValue(fresh.data);
      }
    } finally {
      if (background) refreshInFlight = false;
      else if (version === generation) setBusy(false);
    }
  }
  /**
   * Chrome's settings are saved whole and always to the global layer, then
   * the pane re-reads its own scope: the shared revision changed, and the
   * agent rows must keep showing the scope that is selected.
   */
  async function saveChrome(chrome: ChromeSettings) {
    const connection = app.state.connection?.id;
    if (!connection || !value()?.chrome || busy()) return false;
    const version = ++generation;
    setBusy(true);
    const read = () =>
      window.haarness.api<{ data: SettingsValue }>(connection, "manageResource", [
        "browser-settings",
        { action: "get", scope: scope() },
      ]);
    try {
      await window.haarness.api(connection, "manageResource", [
        "browser-settings",
        { action: "set", chrome, revision: value()?.revision, scope: "global" },
      ]);
      const fresh = await read();
      if (version === generation && connection === app.state.connection?.id) {
        setValue(fresh.data);
        setError("");
      }
      return true;
    } catch (cause) {
      if (version === generation && !cancelled(cause)) {
        setError(errorLine(cause));
        const fresh = await read().catch(() => undefined);
        if (version === generation && fresh) setValue(fresh.data);
      }
      return false;
    } finally {
      if (version === generation) setBusy(false);
    }
  }
  function update(change: (policy: BrowserPolicy) => void) {
    if (!policy()) return;
    const next = structuredClone(policy()!);
    change(next);
    void request({ action: "set", policy: next, revision: value()?.revision });
  }
  createEffect(() => {
    // Metadata refreshes keep the same connection and capability. Reset only
    // when these values actually change, not when their source objects refresh.
    void connectionID();
    void supported();
    void scope();
    generation++;
    setValue(undefined);
    setBusy(false);
    setError("");
    untrack(() => void request({ action: "get" }, true));
  });
  createEffect(() => {
    const connection = app.state.connection?.id;
    setManager(undefined);
    setRecords({ history: [], downloads: [] });
    setQuery("");
    if (connection)
      void window.haarness
        .browserData(connection)
        .then((data) => {
          if (app.state.connection?.id === connection) setRecords(data);
        })
        .catch(() => {});
  });
  async function dataRequest(
    input: Parameters<typeof window.haarness.browserData>[1] = {
      action: "get",
    },
  ) {
    const connection = app.state.connection?.id;
    if (!connection || dataBusy()) return;
    setDataBusy(true);
    try {
      const data = await window.haarness.browserData(connection, input);
      if (connection === app.state.connection?.id) {
        setRecords(data);
        setError("");
      }
    } catch (cause) {
      if (connection === app.state.connection?.id && !cancelled(cause)) setError(errorLine(cause));
    } finally {
      setDataBusy(false);
    }
  }
  const timer = setInterval(() => {
    if (!error()) void request({ action: "get" }, false, true);
    if (manager()) void dataRequest();
  }, 2000);
  onCleanup(() => {
    generation++;
    clearInterval(timer);
  });
  // Each permission cell names itself in full, so no sentence is assembled
  // from translated fragments and every language can order the words itself.
  const accessLabel = (origin: string | undefined, field: keyof BrowserAccess) =>
    origin === undefined
      ? t(
          field === "browsing"
            ? "Default browsing"
            : field === "downloads"
              ? "Default downloads"
              : "Default uploads",
        )
      : t(
          field === "browsing"
            ? "{origin} browsing"
            : field === "downloads"
              ? "{origin} downloads"
              : "{origin} uploads",
          { origin },
        );
  const accessSelect = (
    label: string,
    access: BrowserAccess | undefined,
    field: keyof BrowserAccess,
    change: (value: BrowserDecision) => void,
  ) => (
    <Select
      aria-label={label}
      value={access?.[field] ?? "allow"}
      options={decisions.map((decision) => ({
        ...decision,
        label: t(decision.label),
      }))}
      disabled={!value() || busy()}
      onChange={(decision) => change(decision as BrowserDecision)}
    />
  );
  return (
    <div class="browser-settings">
      <p class="muted">
        {t(
          "Manage browser behavior and agent access. Each chat has a separate browser; localhost uses the connected project’s server.",
        )}
      </p>
      <div class="browser-settings-scope" data-setting="Browser settings scope">
        <Select
          aria-label={t("Browser settings scope")}
          value={scope()}
          onChange={setScope}
          disabled={busy() || !supported()}
          options={[
            ...(projectConfig()
              ? [{ value: "project", label: t("This project") }]
              : []),
            { value: "global", label: t("Global default") },
          ]}
        />
        <span>
          {!value()
            ? t("Open a project with browser settings support.")
            : value()!.policy.enabled
              ? t("Current project: agent control on · {source}", {
                  source: value()!.source,
                })
              : t("Current project: agent control off · {source}", {
                  source: value()!.source,
                })}
        </span>
        <button
          disabled={busy() || !value()?.scopePolicy}
          onClick={() =>
            void request({ action: "reset", revision: value()?.revision })
          }
        >
          {t("Use inherited settings")}
        </button>
      </div>
      <Show when={error()}>
        <p class="notice-inline" role="alert">
          {error()}
        </p>
      </Show>
      <Row
        title="Agent browser control"
        description="Allow Haarness to inspect and control this project’s browser from Desktop or an attached TUI. Manual browsing remains available."
      >
        <input
          type="checkbox"
          role="switch"
          aria-label={t("Agent browser control")}
          checked={policy()?.enabled ?? false}
          disabled={!value() || busy()}
          onChange={(event) => {
            const enabled = event.currentTarget.checked;
            update((policy) => {
              policy.enabled = enabled;
            });
          }}
        />
      </Row>
      <h3>{t("General")}</h3>
      <Row
        title="Web link destination"
        description="Where web links in chats open. Applies to this desktop."
      >
        <Select
          aria-label={t("Web link destination")}
          value={app.state.preferences.browserWebLinks}
          options={[
            { value: "external", label: t("System browser") },
            { value: "browser", label: "Haarness" },
          ]}
          onChange={(value) =>
            app.run(() =>
              app.settings({
                browserWebLinks: value as "external" | "browser",
              }),
            )
          }
        />
      </Row>
      <Row
        title="Local URL destination"
        description="Where localhost and loopback links open. Haarness uses the connected server’s network."
      >
        <Select
          aria-label={t("Local URL destination")}
          value={app.state.preferences.browserLocalLinks}
          options={[
            { value: "external", label: t("System browser") },
            { value: "browser", label: "Haarness" },
          ]}
          onChange={(value) =>
            app.run(() =>
              app.settings({
                browserLocalLinks: value as "external" | "browser",
              }),
            )
          }
        />
      </Row>
      <Row
        title="Show full URL"
        description="Show paths, queries, and fragments in the address bar. Editing always shows the complete URL."
      >
        <input
          type="checkbox"
          role="switch"
          aria-label={t("Show full URL")}
          checked={app.state.preferences.browserShowFullURL}
          onChange={(event) =>
            app.run(() =>
              app.settings({ browserShowFullURL: event.currentTarget.checked }),
            )
          }
        />
      </Row>
      <Row
        title="Show the agent’s cursor"
        description="See where Haarness points, clicks and types while it controls the browser. Turning it off makes browser actions a little faster."
      >
        <input
          type="checkbox"
          role="switch"
          aria-label={t("Show the agent’s cursor")}
          checked={app.state.preferences.browserAgentCursor}
          onChange={(event) =>
            app.run(() =>
              app.settings({ browserAgentCursor: event.currentTarget.checked }),
            )
          }
        />
      </Row>
      <Row
        title="Browsing data"
        description="Clear this project’s tabs, cookies, site storage, cache, history, and retained downloads on this desktop. Saved files and chat attachments remain."
      >
        <button
          disabled={!app.state.connection || dataBusy()}
          onClick={async () => {
            const connection = app.state.connection?.id;
            if (
              (await confirm(
                t("Clear browsing data?"),
                t(
                  "Close this project’s browser tabs and clear their browsing data on this desktop. Files saved separately and chat attachments remain.",
                ),
                { submit: t("Clear browsing data"), danger: true },
              )) &&
              connection === app.state.connection?.id
            )
              await dataRequest({ action: "clear", kind: "all" });
          }}
        >
          {t("Clear browsing data")}
        </button>
      </Row>
      <Row
        title="Browsing history"
        description="View, search, reopen, and remove pages visited in this project’s browser."
      >
        <button
          disabled={!app.state.connection}
          onClick={() => {
            setManager(manager() === "history" ? undefined : "history");
            setQuery("");
            void dataRequest();
          }}
        >
          {t("Manage browsing history")}
        </button>
      </Row>
      <Row
        title="Ask where to save downloads"
        description="Show a save dialog when a browser download completes. Otherwise, save it later from download history."
      >
        <input
          type="checkbox"
          role="switch"
          aria-label={t("Ask where to save downloads")}
          checked={app.state.preferences.browserAskDownloadLocation}
          onChange={(event) =>
            app.run(() =>
              app.settings({
                browserAskDownloadLocation: event.currentTarget.checked,
              }),
            )
          }
        />
      </Row>
      <Row
        title="Download history"
        description="Manage the last 200 retained downloads for this project. Removing an entry removes its retained copy, but keeps files saved separately."
      >
        <button
          disabled={!app.state.connection}
          onClick={() => {
            setManager(manager() === "downloads" ? undefined : "downloads");
            setQuery("");
            void dataRequest();
          }}
        >
          {t("Manage downloads")}
        </button>
      </Row>
      <Show when={manager()}>
        <section
          class="browser-records"
          aria-label={
            manager() === "history"
              ? t("Browsing history")
              : t("Download history")
          }
        >
          <div class="browser-settings-scope">
            <input
              aria-label={t("Search browser records")}
              placeholder={t("Search history…")}
              value={query()}
              onInput={(event) => setQuery(event.currentTarget.value)}
            />
            <button
              disabled={dataBusy()}
              onClick={async () => {
                const kind = manager()!,
                  connection = app.state.connection?.id;
                if (
                  (await confirm(
                    t("Clear history?"),
                    kind === "history"
                      ? t(
                          "Remove this project’s browsing history on this desktop.",
                        )
                      : t(
                          "Remove this project’s retained downloads. Files saved separately remain.",
                        ),
                    { submit: t("Clear history"), danger: true },
                  )) &&
                  connection === app.state.connection?.id
                )
                  await dataRequest({ action: "clear", kind });
              }}
            >
              {t("Clear history")}
            </button>
          </div>
          <Show when={manager() === "history"}>
            <For
              each={records()
                .history.filter((entry) =>
                  `${entry.title} ${entry.url}`
                    .toLowerCase()
                    .includes(query().toLowerCase()),
                )
                .slice(0, 200)}
              fallback={<p>{t("No matching browsing history.")}</p>}
            >
              {(entry) => (
                <div class="browser-record">
                  <div>
                    <strong>{entry.title || entry.url}</strong>
                    <p title={entry.url}>{entry.url}</p>
                    <small>{formatDateTime(entry.time)}</small>
                  </div>
                  <button
                    disabled={!app.state.connection || dataBusy()}
                    onClick={() =>
                      app.run(async () => {
                        const connection = app.state.connection!.id,
                          thread = app.state.selected ?? "";
                        await window.haarness.browserCommand(
                          connection,
                          thread,
                          { type: "tabs.open", url: entry.url },
                        );
                        if (
                          connection === app.state.connection?.id &&
                          thread === (app.state.selected ?? "")
                        ) {
                          await app.closeSettings();
                        }
                      })
                    }
                  >
                    {t("Open")}
                  </button>
                  <button
                    aria-label={t("Remove visit {url}", { url: entry.url })}
                    disabled={dataBusy()}
                    onClick={() =>
                      void dataRequest({
                        action: "remove",
                        kind: "history",
                        id: entry.id,
                      })
                    }
                  >
                    {t("Remove")}
                  </button>
                </div>
              )}
            </For>
            <Show when={records().history.length > 200}>
              <p>
                {t(
                  "Showing up to 200 matches from the last 1,000 visits. Search to narrow the list.",
                )}
              </p>
            </Show>
          </Show>
          <Show when={manager() === "downloads"}>
            <For
              each={records().downloads.filter((entry) =>
                `${entry.name} ${entry.url}`
                  .toLowerCase()
                  .includes(query().toLowerCase()),
              )}
              fallback={<p>{t("No matching downloads.")}</p>}
            >
              {(entry) => (
                <div class="browser-record">
                  <div>
                    <strong>{entry.name}</strong>
                    <p title={entry.url}>{entry.url}</p>
                    <small>
                      {t("{size} bytes", {
                        size: formatNumber(entry.bytes),
                      })}{" "}
                      · {entry.state} · {formatDateTime(entry.time)}
                    </small>
                  </div>
                  <button
                    disabled={dataBusy() || entry.state !== "completed"}
                    onClick={() =>
                      void dataRequest({ action: "save", id: entry.id })
                    }
                  >
                    {t("Save as…")}
                  </button>
                  <button
                    aria-label={t("Remove download {name}", {
                      name: entry.name,
                    })}
                    disabled={dataBusy()}
                    onClick={() =>
                      void dataRequest({
                        action: "remove",
                        kind: "downloads",
                        id: entry.id,
                      })
                    }
                  >
                    {t("Remove")}
                  </button>
                </div>
              )}
            </For>
          </Show>
        </section>
      </Show>
      <ChromeSettingsSection
        view={value()?.chrome}
        busy={busy()}
        save={saveChrome}
      />
      <h3>{t("Agent site permissions")}</h3>
      <p class="muted">
        {t(
          "Site origins include the scheme and port. Use https://*.example.com to include subdomains. Browsing controls page interaction; uploads control files sent to sites; downloads control files retrieved by the agent. Existing tool restrictions still apply.",
        )}
      </p>
      <label class="setting-row" data-setting="Remember site approvals">
        <input type="checkbox" aria-label={t("Remember site approvals")} checked={policy()?.rememberApprovals ?? false}
          disabled={busy() || !value()} onChange={(event) => { const checked = event.currentTarget.checked; void update(policy => { policy.rememberApprovals = checked; }); }} />
        <span>
          {t("Remember site approvals")}{" "}
          <small>
            {t(
              "Allow a site for this chat or folder. Turn off to require approval for every action.",
            )}
          </small>
        </span>
      </label>
      <div data-setting="Agent site permissions" class="browser-permissions">
        <table>
          <thead>
            <tr>
              <th>{t("Site")}</th>
              <th>{t("Browsing")}</th>
              <th>{t("Downloads")}</th>
              <th>{t("Uploads")}</th>
              <th />
            </tr>
          </thead>
          <tbody>
            <tr>
              <th>{t("Default")}</th>
              <For each={["browsing", "downloads", "uploads"] as const}>
                {(field) => (
                  <td>
                    {accessSelect(
                      accessLabel(undefined, field),
                      policy()?.defaults,
                      field,
                      (decision) =>
                        update((policy) => {
                          policy.defaults[field] = decision;
                        }),
                    )}
                  </td>
                )}
              </For>
              <td />
            </tr>
            <For each={policy()?.sites}>
              {(site, index) => (
                <tr>
                  <th>{site.origin}</th>
                  <For each={["browsing", "downloads", "uploads"] as const}>
                    {(field) => (
                      <td>
                        {accessSelect(
                          accessLabel(site.origin, field),
                          site,
                          field,
                          (decision) =>
                            update((policy) => {
                              policy.sites[index()][field] = decision;
                            }),
                        )}
                      </td>
                    )}
                  </For>
                  <td>
                    <button
                      disabled={busy()}
                      aria-label={t("Remove site {origin}", {
                        origin: site.origin,
                      })}
                      onClick={() =>
                        update((policy) => {
                          policy.sites.splice(index(), 1);
                        })
                      }
                    >
                      <Icon name="close" size="sm" />
                    </button>
                  </td>
                </tr>
              )}
            </For>
          </tbody>
        </table>
        <form
          class="browser-settings-scope"
          onSubmit={(event) => {
            event.preventDefault();
            update((policy) => {
              policy.sites.push({
                origin: origin().trim(),
                ...policy.defaults,
              });
            });
          }}
        >
          <input
            aria-label={t("Site origin")}
            placeholder={t("https://example.com or http://localhost:3000")}
            value={origin()}
            onInput={(event) => setOrigin(event.currentTarget.value)}
            disabled={busy() || !value()}
          />
          <button disabled={busy() || !value() || !origin().trim()}>
            {t("Add site")}
          </button>
        </form>
      </div>
      <p class="muted">
        {t(
          "Logins are temporary and separate for each chat. Restarting Desktop or reconnecting the project clears them. Camera, microphone, and device access are blocked.",
        )}
      </p>
    </div>
  );
}

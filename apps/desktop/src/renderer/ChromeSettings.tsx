import { t } from "./i18n";
import {
  For,
  Match,
  Show,
  Switch,
  createEffect,
  createMemo,
  createSignal,
  onCleanup,
  untrack,
} from "solid-js";
import { useApp } from "./state";
import { Select } from "./Select";
import { SettingRow as Row } from "./SettingRow";
import { Icon } from "./ui";
import { ChromeSetupDialog } from "./ChromeSetupDialog";
import {
  CHROME_CAPABILITY,
  chromeConnection,
  chromeFields,
  editableChromeSettings,
  type ChromeAccess,
  type ChromeField,
  type ChromeHost,
  type ChromeHosts,
  type ChromeSettings,
  type ChromeSettingsView,
} from "../chrome";
import type { BrowserDecision } from "../browser-settings";
import "./remote-control.css";
import "./chrome.css";

/** Ask the server for connected browsers this often until one connects,
 * then only now and then (someone may pause or quit Chrome). */
const WAITING_REFRESH = 2000;
const CONNECTED_REFRESH = 10000;
const decisions = [
  { value: "allow", label: "Always allow" },
  { value: "ask", label: "Requires approval" },
  { value: "deny", label: "Block" },
];
const browserChoices = [
  { value: "auto", label: "Automatic" },
  { value: "haarness", label: "Haarness browser" },
  { value: "chrome", label: "Chrome" },
];

/**
 * Settings › Browser › Chrome. Chrome's own settings live on this computer
 * (the server's global layer), so these rows ignore the pane's scope picker;
 * a project can only make them stricter, and says so here. Every row renders
 * even where Chrome cannot work, disabled, with the reason above it.
 */
export function ChromeSettingsSection(props: {
  view?: ChromeSettingsView;
  busy: boolean;
  /** Resolves true once the server saved it. */
  save: (next: ChromeSettings) => Promise<boolean>;
}) {
  const app = useApp();
  const capable = createMemo(
    () => !!app.state.connection?.info.capabilities.includes(CHROME_CAPABILITY),
  );
  const remote = () => !!app.state.connection?.remote;
  const available = () => capable() && !remote();
  const editable = () => available() && !!props.view && !props.busy;
  const settings = createMemo(() => editableChromeSettings(props.view));
  const [hosts, setHosts] = createSignal<ChromeHost[]>([]);
  const [setupOpen, setSetupOpen] = createSignal(false);
  const [origin, setOrigin] = createSignal("");
  const status = createMemo(() => chromeConnection(hosts()));
  const linked = () => status().state !== "none";
  let inFlight = false;
  let polled = 0;
  async function loadHosts() {
    const connection = app.state.connection?.id;
    if (!connection || !available() || inFlight) return;
    inFlight = true;
    polled = Date.now();
    try {
      const value = await window.haarness.api<ChromeHosts>(
        connection,
        "listBrowserHosts",
        [],
      );
      if (connection === app.state.connection?.id) setHosts(value.hosts ?? []);
    } catch {
      if (connection === app.state.connection?.id) setHosts([]);
    } finally {
      inFlight = false;
    }
  }
  createEffect(() => {
    void app.state.connection?.id;
    void available();
    setHosts([]);
    untrack(() => void loadHosts());
  });
  const timer = setInterval(() => {
    const every = linked() ? CONNECTED_REFRESH : WAITING_REFRESH;
    if (Date.now() - polled >= every) void loadHosts();
  }, WAITING_REFRESH);
  onCleanup(() => clearInterval(timer));
  function update(change: (settings: ChromeSettings) => void) {
    if (!editable()) return Promise.resolve(false);
    const next = editableChromeSettings(props.view);
    change(next);
    return props.save(next);
  }
  // Each cell names itself in full, so a reader hears which table and site
  // a decision belongs to, and no sentence is assembled from fragments.
  const cellLabel = (site: string | undefined, field: ChromeField) => {
    if (site === undefined)
      switch (field) {
        case "browsing":
          return t("Chrome default browsing");
        case "downloads":
          return t("Chrome default downloads");
        case "uploads":
          return t("Chrome default uploads");
        case "scripts":
          return t("Chrome default scripts");
      }
    switch (field) {
      case "browsing":
        return t("{origin} browsing in Chrome", { origin: site });
      case "downloads":
        return t("{origin} downloads in Chrome", { origin: site });
      case "uploads":
        return t("{origin} uploads in Chrome", { origin: site });
      case "scripts":
        return t("{origin} scripts in Chrome", { origin: site });
    }
  };
  const accessSelect = (
    site: string | undefined,
    access: ChromeAccess,
    field: ChromeField,
    change: (value: BrowserDecision) => void,
  ) => (
    <Select
      aria-label={cellLabel(site, field)}
      value={access[field]}
      options={decisions.map((decision) => ({
        ...decision,
        label: t(decision.label),
      }))}
      disabled={!editable()}
      onChange={(decision) => change(decision as BrowserDecision)}
    />
  );
  return (
    <>
      <h3>{t("Chrome")}</h3>
      <p class="muted">
        {t("Chrome settings apply to every project on this computer.")}
      </p>
      <Switch>
        <Match when={remote()}>
          <p class="notice-inline chrome-note">
            {t("Chrome works with projects on this computer.")}
          </p>
        </Match>
        <Match when={!capable()}>
          <p class="notice-inline chrome-note">
            {t("This project’s Haarness server can’t use Chrome. Update Haarness, then reopen the project.")}
          </p>
        </Match>
      </Switch>
      <Show when={available() && props.view?.projectRestrictions.length}>
        <div class="notice-inline chrome-note">
          <span>
            <strong>{t("Restricted by this project’s settings")}</strong>
            <For each={props.view!.projectRestrictions}>
              {(restriction) => <span class="chrome-restriction">{restriction}</span>}
            </For>
          </span>
        </div>
      </Show>
      <Row
        title="Chrome"
        description="Agents can use your own Chrome — with your sign-ins — through the Haarness for Chrome extension. Each new site asks first."
      >
        <span
          class="chrome-status"
          classList={{
            live: status().state === "connected" || status().state === "several",
            paused: status().state === "paused" || status().state === "stopped",
          }}
        >
          <span class="remote-dot" aria-hidden="true" />
          <span class="chrome-status-text">
            <Switch fallback={t("Not connected")}>
              <Match when={!available()}>{t("Not available")}</Match>
              <Match when={status().state === "connected" && status()}>
                {(current) => t("Connected · {name}", {
                  name: (current() as { name: string }).name,
                })}
              </Match>
              <Match when={status().state === "several" && status()}>
                {(current) => t("{count} browsers connected", {
                  count: (current() as { count: number }).count,
                })}
              </Match>
              <Match when={status().state === "paused"}>
                {t("Paused in Chrome")}
              </Match>
              <Match when={status().state === "stopped"}>
                {t("Stopped in Chrome")}
              </Match>
            </Switch>
          </span>
        </span>
        <button disabled={!available()} onClick={() => setSetupOpen(true)}>
          {linked() ? t("Manage…") : t("Set up Chrome…")}
        </button>
      </Row>
      <Row
        title="Let agents use Chrome"
        description="Turn off to keep agents out of Chrome on every project. The Haarness browser is not affected."
      >
        <input
          type="checkbox"
          role="switch"
          aria-label={t("Let agents use Chrome")}
          checked={settings().enabled}
          disabled={!editable()}
          onChange={(event) => {
            const enabled = event.currentTarget.checked;
            void update((settings) => {
              settings.enabled = enabled;
            });
          }}
        />
      </Row>
      <Row
        title="Browser for new agent tabs"
        description="Where an agent opens a tab when it doesn’t choose. Automatic uses the Haarness browser when the chat is open in Desktop, otherwise Chrome."
      >
        <Select
          aria-label={t("Browser for new agent tabs")}
          value={settings().defaultBrowser}
          options={browserChoices.map((choice) => ({
            ...choice,
            label: t(choice.label),
          }))}
          disabled={!editable()}
          onChange={(value) =>
            void update((settings) => {
              settings.defaultBrowser = value as ChromeSettings["defaultBrowser"];
            })
          }
        />
      </Row>
      <div
        class="browser-permissions chrome-permissions"
        data-setting="Chrome site permissions"
      >
        <div class="chrome-permissions-head">
          <strong>{t("Chrome site permissions")}</strong>
          <p>
            {t("What agents may do on each site in your Chrome. Scripts lets an agent run its own code in a page, so allow it only where you would let it act as you.")}
          </p>
        </div>
        {/* Narrow panes restack this table (chrome.css), so its roles are
            stated rather than left to the display type. */}
        <table role="table">
          <thead role="rowgroup">
            <tr role="row">
              <th role="columnheader">{t("Site")}</th>
              <th role="columnheader">{t("Browsing")}</th>
              <th role="columnheader">{t("Downloads")}</th>
              <th role="columnheader">{t("Uploads")}</th>
              <th role="columnheader">{t("Scripts")}</th>
              <th role="columnheader" />
            </tr>
          </thead>
          <tbody role="rowgroup">
            <tr role="row">
              <th role="rowheader">{t("Default")}</th>
              <For each={chromeFields}>
                {(field) => (
                  <td role="cell">
                    {accessSelect(undefined, settings().defaults, field, (decision) =>
                      void update((settings) => {
                        settings.defaults[field] = decision;
                      }),
                    )}
                  </td>
                )}
              </For>
              <td role="cell" />
            </tr>
            <For each={settings().sites}>
              {(site, index) => (
                <tr role="row">
                  <th role="rowheader">{site.origin}</th>
                  <For each={chromeFields}>
                    {(field) => (
                      <td role="cell">
                        {accessSelect(site.origin, site, field, (decision) =>
                          void update((settings) => {
                            settings.sites[index()][field] = decision;
                          }),
                        )}
                      </td>
                    )}
                  </For>
                  <td role="cell">
                    <button
                      disabled={!editable()}
                      aria-label={t("Remove Chrome site {origin}", {
                        origin: site.origin,
                      })}
                      onClick={() =>
                        void update((settings) => {
                          settings.sites.splice(index(), 1);
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
            const value = origin().trim();
            if (!value) return;
            void update((settings) => {
              settings.sites.push({ origin: value, ...settings.defaults });
            }).then((saved) => {
              if (saved && origin().trim() === value) setOrigin("");
            });
          }}
        >
          <input
            aria-label={t("New Chrome site")}
            placeholder={t("https://example.com or http://localhost:3000")}
            value={origin()}
            onInput={(event) => setOrigin(event.currentTarget.value)}
            disabled={!editable()}
          />
          <button
            aria-label={t("Add site to Chrome")}
            disabled={!editable() || !origin().trim()}
          >
            {t("Add site")}
          </button>
        </form>
      </div>
      <p class="muted">
        {t("Blocked sites in Agent site permissions are blocked in Chrome too.")}
      </p>
      <Show when={setupOpen()}>
        <ChromeSetupDialog
          close={() => {
            setSetupOpen(false);
            void loadHosts();
          }}
        />
      </Show>
    </>
  );
}

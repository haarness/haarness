import { t, splitAround, errorLine } from "./i18n";
import {
  For,
  Match,
  Show,
  Switch,
  createMemo,
  createSignal,
  onCleanup,
  onMount,
} from "solid-js";
import { Button, Icon, dismissOnBackdrop } from "./ui";
import { cancelled } from "./error-text";
import { useApp } from "./state";
import {
  chromeHostName,
  extensionsPage,
  notIncludedWarning,
  setupBrowsers,
  webStoreLink,
  type ChromeHost,
  type ChromeHosts,
  type ChromeSetupInfo,
} from "../chrome";
import "./connect-provider.css";
import "./chrome.css";

/** How often the dialog asks the project's server whether Chrome connected. */
const WAITING_REFRESH = 2000;
type StepState = "done" | "busy" | "current" | "failed" | "pending";

/**
 * "Use Chrome with Haarness": register the native host that Chrome starts,
 * walk through loading the extension, and wait for it to connect to this
 * project's server. Every step reads live state, so reopening it later shows
 * where things stand.
 */
export function ChromeSetupDialog(props: { close: () => void }) {
  const app = useApp();
  const connection = app.state.connection!.id;
  const [setup, setSetup] = createSignal<ChromeSetupInfo>();
  const [setupError, setSetupError] = createSignal("");
  const [running, setRunning] = createSignal(false);
  const [hosts, setHosts] = createSignal<ChromeHost[]>([]);
  const [actionError, setActionError] = createSignal("");
  let dialog!: HTMLDialogElement;
  let alive = true;

  async function runSetup() {
    if (running()) return;
    setRunning(true);
    setSetupError("");
    try {
      const value = await window.haarness.chromeSetup("setup");
      if (alive) setSetup(value);
    } catch (cause) {
      if (alive && !cancelled(cause)) setSetupError(errorLine(cause));
    } finally {
      if (alive) setRunning(false);
    }
  }
  async function poll() {
    try {
      const value = await window.haarness.api<ChromeHosts>(
        connection,
        "listBrowserHosts",
        [],
      );
      if (alive) setHosts(value.hosts ?? []);
    } catch {
      /* A server without Chrome support keeps the dialog waiting. */
    }
  }
  const close = () => {
    if (!dialog.isConnected || !dialog.open) return;
    dialog.close();
    props.close();
  };
  onMount(() => {
    dialog.showModal();
    void runSetup();
    void poll();
    const timer = setInterval(() => {
      if (!linked()) void poll();
    }, WAITING_REFRESH);
    onCleanup(() => {
      alive = false;
      clearInterval(timer);
    });
  });
  const browser = createMemo(() =>
    hosts().find((host) => host.kind === "chrome"),
  );
  const linked = () => !!browser();
  const browsers = createMemo(() => {
    const info = setup();
    return info ? setupBrowsers(info) : { ready: [], unsupported: [] };
  });
  const prepared = () => !!setup() && browsers().ready.length > 0;
  /** The setup command's notes. Once step 2 is reached with nothing to add,
   * it says so in the reader's language, and the command's English note
   * would only repeat it. */
  const warnings = () => {
    const info = setup();
    if (!info) return [];
    const said = (prepared() || linked()) && !webStoreLink(info) && !info.extension;
    return (info.warnings ?? []).filter((warning) => !(said && notIncludedWarning(warning)));
  };
  const first = (): StepState =>
    running()
      ? "busy"
      : prepared() || linked()
        ? "done"
        : setupError() || setup()
          ? "failed"
          : "pending";
  const second = (): StepState =>
    linked() ? "done" : first() === "done" ? "current" : "pending";
  const third = (): StepState =>
    linked() ? "done" : first() === "done" ? "busy" : "pending";
  /** The browser named in the steps: Google Chrome when it is set up. */
  const target = () =>
    browsers().ready.find((item) => item.browser === "chrome") ??
    browsers().ready[0];
  const keys = () => (app.state.platform === "darwin" ? "⌘⇧G" : "Ctrl+L");
  const revealLabel = () =>
    app.state.platform === "darwin"
      ? t("Reveal in Finder")
      : app.state.platform === "win32"
        ? t("Show in Explorer")
        : t("Show in file manager");
  /** Copy, reveal and open report a failure here, not behind the dialog. */
  async function attempt(action: () => Promise<unknown>) {
    setActionError("");
    try {
      await action();
    } catch (cause) {
      if (alive && !cancelled(cause)) setActionError(errorLine(cause));
    }
  }
  const copy = (text: string) =>
    attempt(async () => {
      await navigator.clipboard.writeText(text);
      app.notify(t("Copied"));
    });
  return (
    <dialog
      ref={dialog}
      class="dialog editor-dialog chrome-dialog"
      aria-label={t("Use Chrome with Haarness")}
      onCancel={(event) => {
        event.preventDefault();
        close();
      }}
      {...dismissOnBackdrop(close)}
    >
      <header>
        <div>
          <h2>{t("Use Chrome with Haarness")}</h2>
          <p class="muted">
            {t("Agents work in your own Chrome, with your sign-ins. Each new site asks you first.")}
          </p>
        </div>
        <Button icon="close" label={t("Close dialog")} onClick={close} />
      </header>
      <ol class="chrome-steps">
        <li class="chrome-step" data-state={first()}>
          <StepMark state={first()} index={1} />
          <div class="chrome-step-body">
            <strong>{t("Chrome can start Haarness")}</strong>
            <Switch>
              <Match when={running()}>
                <p class="muted">{t("Setting up…")}</p>
              </Match>
              <Match when={setupError()}>
                <p class="form-error" role="alert">
                  {setupError()}
                </p>
                <div class="chrome-actions">
                  <button type="button" onClick={() => void runSetup()}>
                    {t("Try again")}
                  </button>
                </div>
              </Match>
              <Match when={setup()}>
                <Show
                  when={browsers().ready.length}
                  fallback={
                    <p class="form-error" role="alert">
                      {t("Haarness couldn’t find a browser to set up. Install Google Chrome, then try again.")}
                    </p>
                  }
                >
                  <p class="muted">{t("Haarness is registered with:")}</p>
                </Show>
                <ul class="chrome-browsers">
                  <For each={browsers().ready}>
                    {(item) => (
                      <li>
                        <Icon name="check" size="sm" />
                        <span>{item.name}</span>
                      </li>
                    )}
                  </For>
                  <For each={browsers().unsupported}>
                    {(item) => (
                      <li class="unsupported">
                        <Icon name="alert" size="sm" />
                        <span>
                          {item.name}
                          <Show when={item.note}>
                            <small>{item.note}</small>
                          </Show>
                        </span>
                      </li>
                    )}
                  </For>
                </ul>
                <For each={warnings()}>
                  {(warning) => <p class="chrome-warning">{warning}</p>}
                </For>
                <Show when={!browsers().ready.length}>
                  <div class="chrome-actions">
                    <button type="button" onClick={() => void runSetup()}>
                      {t("Try again")}
                    </button>
                  </div>
                </Show>
              </Match>
            </Switch>
          </div>
        </li>
        <li class="chrome-step" data-state={second()}>
          <StepMark state={second()} index={2} />
          <div class="chrome-step-body">
            <strong>{t("Add Haarness for Chrome")}</strong>
            <Switch>
              <Match when={linked()}>
                <p class="muted">{t("Haarness for Chrome is installed.")}</p>
              </Match>
              <Match when={second() === "current" && setup()}>
                {(info) => (
                  <Show
                    when={webStoreLink(info())}
                    fallback={
                      <Show
                        when={info().extension}
                        fallback={
                          <p class="form-error" role="alert">
                            {t("This Haarness installation doesn’t include Haarness for Chrome.")}
                          </p>
                        }
                      >
                        {(extension) => (
                          <ol class="chrome-substeps">
                            <li>
                              <span>
                                {t("Open this page in {browser}:", {
                                  browser: target()?.name ?? "Google Chrome",
                                })}
                              </span>
                              <span class="chrome-copy">
                                <code>{extensionsPage(info())}</code>
                                <Button
                                  icon="copy"
                                  size="sm"
                                  label={t("Copy page address")}
                                  onClick={() => void copy(extensionsPage(info()))}
                                />
                              </span>
                            </li>
                            <li>
                              <span>{t("Turn on Developer mode, and keep it on.")}</span>
                            </li>
                            <li>
                              <span>
                                {splitAround(
                                  t("Click Load unpacked, press {keys}, and paste this folder path:"),
                                  () => <kbd>{keys()}</kbd>,
                                  "{keys}",
                                )}
                              </span>
                              <span class="chrome-copy">
                                <code>{extension().path}</code>
                                <Button
                                  icon="copy"
                                  size="sm"
                                  label={t("Copy folder path")}
                                  onClick={() => void copy(extension().path)}
                                />
                              </span>
                              <span class="chrome-reveal">
                                <button
                                  type="button"
                                  onClick={() => void attempt(() => window.haarness.chromeReveal())}
                                >
                                  <Icon name="folder-open" size="sm" />
                                  {revealLabel()}
                                </button>
                                <small>{t("Or drag the folder onto the picker.")}</small>
                              </span>
                            </li>
                            <li>
                              <span>{t("Pin Haarness to the toolbar.")}</span>
                            </li>
                          </ol>
                        )}
                      </Show>
                    }
                  >
                    {(url) => (
                      <>
                        <p class="muted">
                          {t("Add Haarness for Chrome from the Chrome Web Store, then pin it to the toolbar.")}
                        </p>
                        <div class="chrome-actions">
                          <button
                            type="button"
                            onClick={() => void attempt(() => window.haarness.openExternal(url()))}
                          >
                            <Icon name="external" size="sm" />
                            {t("Open Chrome Web Store")}
                          </button>
                        </div>
                      </>
                    )}
                  </Show>
                )}
              </Match>
            </Switch>
          </div>
        </li>
        <li class="chrome-step" data-state={third()} aria-live="polite">
          <StepMark state={third()} index={3} />
          <div class="chrome-step-body">
            <Show
              when={browser()}
              fallback={<strong>{t("Waiting for Chrome…")}</strong>}
            >
              {(host) => (
                <>
                  <strong>
                    {t("Connected to {name}", { name: chromeHostName(host()) })}
                  </strong>
                  <p class="muted">
                    {t("Try it: ask an agent to open a website in Chrome.")}
                  </p>
                </>
              )}
            </Show>
            <Show when={third() === "busy"}>
              <p class="muted">
                {t("Haarness connects as soon as Chrome loads the extension.")}
              </p>
            </Show>
          </div>
        </li>
      </ol>
      <Show when={actionError()}>
        <p class="form-error" role="alert">
          {actionError()}
        </p>
      </Show>
      <footer>
        <button
          type="button"
          classList={{ primary: linked() }}
          onClick={close}
        >
          {linked() ? t("Done") : t("Close")}
        </button>
      </footer>
    </dialog>
  );
}

/** A step's number, or what became of it; the words are for screen readers. */
function StepMark(props: { state: StepState; index: number }) {
  return (
    <>
      <span class="chrome-step-mark" aria-hidden="true">
        <Switch fallback={props.index}>
          <Match when={props.state === "done"}>
            <Icon name="check" size="sm" />
          </Match>
          <Match when={props.state === "failed"}>
            <Icon name="alert" size="sm" />
          </Match>
          <Match when={props.state === "busy"}>
            <span class="connect-spinner" />
          </Match>
        </Switch>
      </span>
      <Show when={props.state === "done" || props.state === "failed"}>
        <span class="sr-only">
          {props.state === "done" ? t("Done") : t("Failed")}
        </span>
      </Show>
    </>
  );
}

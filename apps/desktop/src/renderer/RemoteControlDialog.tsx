import { t, errorLine } from "./i18n";
import {
  Match,
  Show,
  Switch,
  createSignal,
  onCleanup,
  onMount,
} from "solid-js";
import { Button, Icon, dismissOnBackdrop } from "./ui";
import { cancelled } from "./error-text";
import { useApp } from "./state";
import { sessionKey } from "../projects";
import {
  isLive,
  parseShare,
  qrPath,
  type RemoteAction,
  type RemoteShare,
} from "./remote-control";
import "./remote-control.css";

/** How often the dialog re-asks Cloud while the connector comes up, and once
 * it is live (someone may stop sharing from the browser). */
const CONNECTING_REFRESH = 2000;
const LIVE_REFRESH = 10000;

/**
 * `/remote-control`: share this chat through Haarness Cloud and show the link
 * and a QR code to continue it on a phone. The chat keeps running on this
 * computer; the server runs the connector.
 */
export function RemoteControlDialog(props: {
  action: RemoteAction;
  close: () => void;
}) {
  const app = useApp();
  const connection = app.state.connection!.id;
  const thread = app.state.selected;
  const key = sessionKey(connection, thread);
  const [share, setShare] = createSignal<RemoteShare | undefined>(
    app.state.remoteShares[key],
  );
  const [busy, setBusy] = createSignal<RemoteAction | "">("");
  const [error, setError] = createSignal("");
  let dialog!: HTMLDialogElement;
  let alive = true;
  let refreshed = Date.now();

  async function request(action: RemoteAction) {
    if (busy() === "start" || busy() === "stop") return;
    setBusy(action);
    if (action !== "view") setError("");
    refreshed = Date.now();
    try {
      const view = await window.haarness.api(connection, "manageResource", [
        "remote-control",
        { action, threadId: thread },
      ]);
      if (!alive) return;
      const value = parseShare(view);
      setShare(value);
      app.setRemoteShare(key, value);
      if (action === "stop" && !value.error)
        app.notify(t("Stopped sharing. This chat stays on this computer."));
    } catch (cause) {
      if (!alive || action === "view" || cancelled(cause)) return;
      setError(errorLine(cause));
    } finally {
      if (alive) setBusy("");
    }
  }
  const close = () => {
    if (!dialog.isConnected || !dialog.open) return;
    dialog.close();
    props.close();
  };
  onMount(() => {
    dialog.showModal();
    void request(props.action);
    const timer = setInterval(() => {
      const current = share();
      if (busy() || !current) return;
      const every =
        current.state === "connecting"
          ? CONNECTING_REFRESH
          : current.state === "on"
            ? LIVE_REFRESH
            : 0;
      if (every && Date.now() - refreshed >= every) void request("view");
    }, 500);
    // Signing in from here opens the account dialog; share as soon as it
    // reports the new account.
    const off = window.haarness.onEvent((event) => {
      const data = event.data as { action?: string };
      if (
        event.kind === "menu" &&
        data.action === "account-changed" &&
        share()?.state === "signed_out"
      )
        void request("start");
    });
    onCleanup(() => {
      alive = false;
      clearInterval(timer);
      off();
    });
  });
  const live = () => isLive(share());
  const size = () => share()?.qr.length ?? 0;
  async function copy() {
    const url = share()?.url;
    if (!url) return;
    await navigator.clipboard.writeText(url);
    app.notify(t("Link copied"));
  }
  return (
    <dialog
      ref={dialog}
      class="dialog editor-dialog remote-dialog"
      aria-label={t("Remote control")}
      onCancel={(event) => {
        event.preventDefault();
        close();
      }}
      {...dismissOnBackdrop(close)}
    >
      <header>
        <div>
          <h2>{t("Remote control")}</h2>
          <p class="muted">
            {t("Continue this chat from your phone or any browser")}
          </p>
        </div>
        <Button icon="close" label={t("Close dialog")} onClick={close} />
      </header>
      <Show when={error()}>
        <p class="form-error" role="alert">
          {error()}
        </p>
      </Show>
      <Switch
        fallback={
          <p class="remote-waiting muted" aria-live="polite">
            <span class="connect-spinner" aria-hidden="true" />
            {t("Checking…")}
          </p>
        }
      >
        <Match when={busy() === "stop"}>
          <p class="remote-waiting muted" aria-live="polite">
            <span class="connect-spinner" aria-hidden="true" />
            {t("Stopping…")}
          </p>
        </Match>
        <Match when={busy() === "start" && !live()}>
          <p class="remote-waiting muted" aria-live="polite">
            <span class="connect-spinner" aria-hidden="true" />
            {t("Sharing this chat with Haarness Cloud…")}
          </p>
        </Match>
        <Match when={live() && share()}>
          {(current) => (
            <>
              <div class="remote-live">
                <Show when={size() > 0 && current().url}>
                  <svg
                    class="remote-qr"
                    viewBox={`-2 -2 ${size() + 4} ${size() + 4}`}
                    shape-rendering="crispEdges"
                    role="img"
                    aria-label={t("QR code for this chat's link")}
                  >
                    <rect x="-2" y="-2" width={size() + 4} height={size() + 4} fill="#fff" />
                    <path d={qrPath(current().qr)} fill="#000" />
                  </svg>
                </Show>
                <div class="remote-details">
                  <p
                    class="remote-status"
                    classList={{ live: current().state === "on" }}
                    aria-live="polite"
                  >
                    <span class="remote-dot" aria-hidden="true" />
                    {current().state === "on"
                      ? t("Live in Haarness Cloud")
                      : t("Connecting to Haarness Cloud…")}
                  </p>
                  <Show when={current().chat}>
                    <p class="remote-chat">
                      <strong>{current().chat}</strong>
                      <Show when={current().computer}>
                        {(computer) => (
                          <span class="muted">
                            {t("on {computer}", { computer: computer() })}
                          </span>
                        )}
                      </Show>
                    </p>
                  </Show>
                  <p>
                    {t("Scan the code with your phone, or open the link in any browser signed in to Haarness Cloud.")}
                  </p>
                  <Show when={current().url}>
                    {(url) => (
                      <div class="remote-link">
                        <code>{url()}</code>
                        <Button icon="copy" size="sm" label={t("Copy link")} onClick={() => void app.run(copy)} />
                      </div>
                    )}
                  </Show>
                  <p class="small">
                    {t("This chat keeps running on this computer, with its files and model keys, until you stop sharing.")}
                  </p>
                </div>
              </div>
              <Show when={current().error}>
                <p class="form-error">{current().error}</p>
              </Show>
              <footer>
                <button
                  type="button"
                  class="remote-stop"
                  disabled={!!busy()}
                  onClick={() => void request("stop")}
                >
                  {t("Stop sharing")}
                </button>
                <span class="remote-spacer" />
                <Show when={current().url}>
                  {(url) => (
                    <button
                      type="button"
                      onClick={() => void window.haarness.openExternal(url())}
                    >
                      <Icon name="external" size="sm" />
                      {t("Open in browser")}
                    </button>
                  )}
                </Show>
                <button type="button" class="primary" onClick={close}>
                  {t("Done")}
                </button>
              </footer>
            </>
          )}
        </Match>
        <Match when={share()?.state === "signed_out"}>
          <p class="remote-lead">
            {t("Sign in to Haarness Cloud to continue this chat from your phone or any browser.")}
          </p>
          <footer>
            <button type="button" onClick={close}>
              {t("Cancel")}
            </button>
            <button
              type="button"
              class="primary"
              onClick={() => window.dispatchEvent(new CustomEvent("haarness:account"))}
            >
              {t("Sign in to Haarness Cloud")}
            </button>
          </footer>
        </Match>
        <Match when={share()}>
          {(current) => (
            <>
              <p class="remote-lead">
                {current().state === "stopped"
                  ? t("Sharing was stopped from Haarness Cloud. Share again for a new link.")
                  : t("Share it to get a link and a QR code. It keeps running on this computer, with its files and model keys.")}
              </p>
              <Show when={current().error}>
                <p class="form-error">{current().error}</p>
              </Show>
              <footer>
                <button type="button" onClick={close}>
                  {t("Cancel")}
                </button>
                <button
                  type="button"
                  class="primary"
                  disabled={!!busy()}
                  onClick={() => void request("start")}
                >
                  {t("Share this chat")}
                </button>
              </footer>
            </>
          )}
        </Match>
      </Switch>
    </dialog>
  );
}

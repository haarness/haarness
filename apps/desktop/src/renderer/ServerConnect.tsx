import { t } from "./i18n";
import { createSignal, onMount, Show } from "solid-js";
import { useApp } from "./state";
import { Button, dismissOnBackdrop } from "./ui";
import "./server-connect.css";

const [opened, setOpened] = createSignal(false);
export function openServerConnection() {
  setOpened(true);
}
export function ServerConnect() {
  return (
    <Show when={opened()}>
      <ConnectionForm />
    </Show>
  );
}
function ConnectionForm() {
  const app = useApp();
  let dialog!: HTMLDialogElement;
  const [url, setUrl] = createSignal("");
  const [name, setName] = createSignal("");
  const [username, setUsername] = createSignal("");
  const [password, setPassword] = createSignal("");
  const [authentication, setAuthentication] = createSignal(false);
  const [visible, setVisible] = createSignal(false);
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal("");
  const close = () => {
    if (!dialog.isConnected || !dialog.open) return;
    dialog.close();
    setOpened(false);
  };
  onMount(() => dialog.showModal());
  const submit = async (event: SubmitEvent) => {
    event.preventDefault();
    if (busy()) return;
    setError("");
    let endpoint: URL;
    try {
      endpoint = new URL(url().trim());
      if (
        !["http:", "https:"].includes(endpoint.protocol) ||
        endpoint.username ||
        endpoint.password ||
        endpoint.search ||
        endpoint.hash ||
        !["", "/"].includes(endpoint.pathname)
      )
        throw new Error();
    } catch {
      setError(t("Enter a server address such as http://localhost:4096."));
      return;
    }
    setBusy(true);
    try {
      const connected = await app.connect(
        {
          url: endpoint.origin,
          label: name().trim(),
          username: authentication() ? username().trim() : "",
          password: authentication() ? password() : "",
        },
        false,
        true,
      );
      if (connected) {
        setBusy(false);
        close();
      } else
        setError(t("The connection was cancelled. Try again when you’re ready."));
    } catch (failure) {
      const message =
        failure instanceof Error ? failure.message : String(failure);
      setError(
        /401|unauthori[sz]ed|authentication required/i.test(message)
          ? t(
              "Couldn’t sign in. Check your username and password, or ask the server owner for access.",
            )
          : /403|forbidden/i.test(message)
            ? t(
                "This server isn’t allowing the connection. Check its access settings.",
              )
            : /https|SSH tunnel/.test(message)
              ? t(
                  "Use an HTTPS address for a remote server, or connect through a local SSH tunnel.",
                )
              : /incompatible/i.test(message)
                ? t(
                    "This server needs a compatible version of Haarness. Update it and try again.",
                  )
                : t(
                    "Couldn’t connect. Check the address and make sure the server is running.",
                  ),
      );
    } finally {
      setBusy(false);
    }
  };
  return (
    <dialog
      ref={dialog}
      class="dialog server-connect"
      aria-label={t("Connect to server")}
      onCancel={(event) => {
        event.preventDefault();
        close();
      }}
      {...dismissOnBackdrop(close)}
    >
      <form onSubmit={submit} aria-busy={busy()}>
        <header>
          <h2>{t("Connect to server")}</h2>
          <Button
            icon="close"
            label={t("Close dialog")}
            onClick={close}
          />
        </header>
        <p class="muted">
          {t("Access your projects and chats on another Haarness server.")}
        </p>
        <fieldset disabled={busy()}>
          <label class="field">
            <span>{t("Server URL")}</span>
            <input
              autofocus
              type="url"
              required
              placeholder={t("http://localhost:4096")}
              value={url()}
              onInput={(event) => setUrl(event.currentTarget.value)}
              spellcheck={false}
              autocomplete="url"
            />
          </label>
          <label class="field">
            <span>
              {t("Name")} <small>{t("optional")}</small>
            </span>
            <input
              aria-label={t("Name")}
              placeholder={t("Use the server address")}
              value={name()}
              onInput={(event) => setName(event.currentTarget.value)}
            />
          </label>
          <details
            open={authentication()}
            onToggle={(event) => setAuthentication(event.currentTarget.open)}
          >
            <summary>
              {t("Username and password")} <span>{t("optional")}</span>
            </summary>
            <p class="muted">{t("Only needed if the server has a password.")}</p>
            <label class="field">
              <span>{t("Username")}</span>
              <input
                placeholder="haarness"
                autocomplete="username"
                value={username()}
                onInput={(event) => setUsername(event.currentTarget.value)}
              />
            </label>
            <label class="field">
              <span>{t("Password")}</span>
              <div class="server-password">
                <input
                  aria-label={t("Password")}
                  type={visible() ? "text" : "password"}
                  autocomplete="current-password"
                  value={password()}
                  onInput={(event) => setPassword(event.currentTarget.value)}
                />
                <button
                  type="button"
                  aria-label={
                    visible() ? t("Hide password") : t("Show password")
                  }
                  aria-pressed={visible()}
                  onClick={() => setVisible(!visible())}
                >
                  {visible() ? t("Hide") : t("Show")}
                </button>
              </div>
            </label>
          </details>
        </fieldset>
        <Show when={error()}>
          <p class="server-connect-error" role="alert">
            {error()}
          </p>
        </Show>
        <footer>
          <button type="button" onClick={close}>
            {t("Cancel")}
          </button>
          <button
            type="submit"
            class="primary"
            disabled={busy() || !url().trim()}
          >
            {busy() ? t("Connecting…") : t("Connect")}
          </button>
        </footer>
      </form>
    </dialog>
  );
}

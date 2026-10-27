import { t, splitAround } from "./i18n";
import { AccountMenu } from "./AccountMenu";
import type { ManagementView } from "@haarness/sdk";
import {
  For,
  Show,
  Switch,
  Match,
  createMemo,
  createSignal,
  onCleanup,
  onMount,
} from "solid-js";
import { Button, ErrorNotice, Icon, confirm, dismissOnBackdrop } from "./ui";
import { cancelled } from "./error-text";
import { ProviderIcon } from "./ProviderIcon";
import { ProviderForm } from "./ProviderForm";
import { RouterConnectionDialog } from "./RouterConnectionDialog";
import { useApp } from "./state";
import {
  connectionSummary,
  fallbackCatalog,
  filterProviders,
  groupProviders,
  keyLabel,
  keyWarning,
  parseCatalog,
  parseLogin,
  usableMethods,
  type ConnectMethod,
  type ConnectProvider as Provider,
  type KeyWarning,
  type LoginData,
} from "./connect-provider";
import "./connect-provider.css";

type Step =
  | { kind: "pick" }
  | { kind: "method"; provider: Provider }
  | { kind: "key"; provider: Provider }
  | { kind: "login"; provider: Provider; login: LoginData; opened: boolean }
  /** The key is saved, but the provider could not be asked about it. */
  | { kind: "saved"; provider: Provider; warning: KeyWarning };

/** How often the wait screen asks the server whether the sign-in finished. */
const POLL_MS = 2000;

/**
 * The one door to a working model: pick a provider, sign in or paste a key,
 * and the composer has a model when the dialog closes. Everything goes through
 * the server's management API, so it works the same for a remote server.
 */
export function ConnectProvider(props: {
  close: () => void;
  /** Skip the picker and start on this provider. */
  provider?: string;
  /** Nothing is connected yet: welcome copy instead of the plain title. */
  onboarding?: boolean;
}) {
  const app = useApp();
  const [step, setStep] = createSignal<Step>({ kind: "pick" });
  const [catalog, setCatalog] = createSignal<Provider[]>([]);
  const [loading, setLoading] = createSignal(true);
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal("");
  const [query, setQuery] = createSignal("");
  const [key, setKey] = createSignal("");
  const [custom, setCustom] = createSignal(false);
  const [router, setRouter] = createSignal(false);
  let dialog!: HTMLDialogElement;
  let search: HTMLInputElement | undefined;
  let keyInput: HTMLInputElement | undefined;
  let doneButton: HTMLButtonElement | undefined;
  let poll: ReturnType<typeof setInterval> | undefined;
  let generation = 0;

  const local = () => !app.state.connection?.remote;
  const manage = (input: Record<string, unknown>) =>
    app.api<ManagementView>("manageResource", "providers", input);

  async function load() {
    const current = ++generation;
    setLoading(true);
    try {
      const view = await manage({ action: "connect" });
      if (current !== generation) return;
      const rows = parseCatalog(view);
      setCatalog((rows.length ? rows : fallbackCatalog(app.state.providers)).filter(row => row.id !== "haarness"));
    } catch {
      if (current !== generation) return;
      setCatalog(fallbackCatalog(app.state.providers).filter(row => row.id !== "haarness"));
    } finally {
      if (current === generation) setLoading(false);
    }
    if (props.provider) {
      const match = catalog().find((row) => row.id === props.provider);
      if (match) choose(match);
    }
  }

  onMount(() => {
    dialog.showModal();
    void load();
  });
  onCleanup(() => {
    generation++;
    stopPolling();
    const current = step();
    if (current.kind === "login" && current.login.state === "pending")
      void manage({ action: "login_cancel", id: current.login.id }).catch(() => {});
  });

  const close = () => {
    if (!dialog.isConnected || !dialog.open) return;
    dialog.close();
    props.close();
  };

  function back() {
    const current = step();
    setError("");
    if (current.kind === "login" && current.login.state === "pending")
      void manage({ action: "login_cancel", id: current.login.id }).catch(() => {});
    stopPolling();
    if (current.kind === "pick") return close();
    if (current.kind === "saved") return replaceKey(current.provider);
    if (current.kind === "key" && usableMethods(current.provider, local()).length > 1)
      return setStep({ kind: "method", provider: current.provider });
    if (current.kind === "login")
      return setStep({ kind: "method", provider: current.provider });
    setStep({ kind: "pick" });
    queueMicrotask(() => search?.focus());
  }

  function choose(provider: Provider) {
    setError("");
    const methods = usableMethods(provider, local());
    if (!methods.length) {
      setError(provider.note ?? t("This provider cannot be connected from here."));
      return;
    }
    if (methods.length === 1) return void start(provider, methods[0]);
    setStep({ kind: "method", provider });
  }

  function replaceKey(provider: Provider) {
    setError("");
    setKey("");
    setStep({ kind: "key", provider });
    queueMicrotask(() => keyInput?.focus());
  }

  async function start(provider: Provider, method: ConnectMethod) {
    setError("");
    if (method.kind === "api") return replaceKey(provider);
    const currentGeneration = generation;
    setBusy(true);
    try {
      const view = await manage({ action: "login", name: provider.id, method: method.id });
      const login = parseLogin(view);
      if (currentGeneration !== generation) {
        if (login) await manage({ action: "login_cancel", id: login.id });
        return;
      }
      if (!login || !login.url) throw new Error(t("The server did not start a sign-in."));
      let opened = false;
      try {
        await window.haarness.openExternal(login.url);
        opened = true;
      } catch {
        opened = false;
      }
      if (currentGeneration !== generation) {
        await manage({ action: "login_cancel", id: login.id });
        return;
      }
      setStep({ kind: "login", provider, login, opened });
      startPolling(login.id, provider);
    } catch (cause) {
      if (!cancelled(cause)) setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setBusy(false);
    }
  }

  function startPolling(id: string, provider: Provider) {
    stopPolling();
    poll = setInterval(() => void check(id, provider), POLL_MS);
  }
  function stopPolling() {
    if (poll) clearInterval(poll);
    poll = undefined;
  }

  async function check(id: string, provider: Provider) {
    const current = step();
    if (current.kind !== "login" || current.login.id !== id) return stopPolling();
    try {
      const view = await manage({ action: "login_status", id });
      const login = parseLogin(view);
      if (!dialog.isConnected || !dialog.open || !login) return;
      if (login.state === "pending") return;
      stopPolling();
      if (login.state === "connected") return void finish(provider);
      setStep({ ...current, login });
    } catch (cause) {
      stopPolling();
      setStep({
        ...current,
        login: {
          ...current.login,
          state: "failed",
          error: cause instanceof Error ? cause.message : String(cause),
        },
      });
    }
  }

  async function submitKey(event: Event) {
    event.preventDefault();
    const current = step();
    if (current.kind !== "key" || busy()) return;
    const value = key().trim();
    if (!value) {
      setError(t("Paste a key first."));
      keyInput?.focus();
      return;
    }
    setBusy(true);
    setError("");
    try {
      const view = await manage({ action: "credential", name: current.provider.id, key: value });
      setKey("");
      const warning = keyWarning(view);
      if (warning) await keptUnchecked(current.provider, warning);
      else await finish(current.provider);
    } catch (cause) {
      if (!cancelled(cause)) setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setBusy(false);
    }
  }

  async function finish(provider: Provider) {
    setBusy(true);
    try {
      await app.catalogs(true);
    } catch {
      // The credential is saved; a slow catalog refresh must not undo that.
    } finally {
      setBusy(false);
    }
    app.notify(t("{name} is connected", { name: provider.name }));
    close();
  }

  /**
   * The server kept the key without being able to ask the provider about it
   * (unreachable, or a gateway that guards its model list). Models refresh
   * as for any connection, but the dialog stays to say so instead of
   * announcing success, and offers to replace the key.
   */
  async function keptUnchecked(provider: Provider, warning: KeyWarning) {
    try {
      await app.catalogs(true);
    } catch {
      // The credential is saved; a slow catalog refresh must not hide that.
    }
    // Closed while the server waited on the provider (up to 8 s): say it
    // where the reader still looks, never as "connected".
    if (!dialog.isConnected || !dialog.open)
      return app.notify(t("Saved the {name} key, but it could not be checked", { name: provider.name }));
    setStep({ kind: "saved", provider, warning });
    queueMicrotask(() => doneButton?.focus());
  }

  const title = createMemo(() => {
    const current = step();
    if (current.kind === "pick")
      return props.onboarding ? t("Welcome to Haarness") : t("Connect a provider");
    if (current.kind === "login")
      return t("Sign in to {name}", { name: current.provider.name });
    return t("Connect {name}", { name: current.provider.name });
  });
  const subtitle = createMemo(() => {
    const current = step();
    // The welcome names the choices in the order the dialog offers them:
    // the Haarness Cloud card (local servers only), then the providers.
    if (current.kind === "pick")
      return props.onboarding
        ? local()
          ? t("Pick how Haarness gets its models: your Haarness Cloud account, a ChatGPT subscription, or an API key from OpenAI, Anthropic, Google and more.")
          : t("Pick how Haarness gets its models: a ChatGPT subscription, or an API key from OpenAI, Anthropic, Google and more.")
        : local()
          ? t("Sign in or add an API key. Keys stay on this computer.")
          : t("Sign in or add an API key. Keys stay on this Haarness server.");
    if (current.kind === "method") return t("How do you want to connect?");
    if (current.kind === "key") return current.provider.note ?? "";
    return "";
  });
  const filtered = createMemo(() => groupProviders(filterProviders(catalog(), query())));
  const status = (provider: Provider) => {
    if (provider.connected) {
      const { how, models } = connectionSummary(provider);
      return [
        t("Connected"),
        how ? t(how) : "",
        models === 1 ? t("1 model") : models ? t("{count} models", { count: models }) : "",
      ]
        .filter(Boolean)
        .join(" · ");
    }
    const oauth = provider.methods.find((method) => method.kind === "oauth");
    if (oauth) return `${oauth.label} · ${t("or use an API key")}`;
    if (!provider.methods.length) return provider.note ?? t("Not available");
    return t("API key");
  };

  return (
    <dialog
      ref={dialog}
      class="dialog editor-dialog connect-dialog"
      classList={{ "connect-picking": step().kind === "pick" }}
      aria-label={title()}
      onCancel={(event) => {
        event.preventDefault();
        if (step().kind === "pick" || step().kind === "saved") close();
        else back();
      }}
      {...dismissOnBackdrop(close)}
    >
      <header>
        <Show when={step().kind !== "pick" && step().kind !== "saved"}>
          <Button icon="chevron" class="connect-back" label={t("Back")} onClick={back} />
        </Show>
        <div>
          <h2>{title()}</h2>
          <Show when={subtitle()}>
            <p class="muted">{subtitle()}</p>
          </Show>
        </div>
        <Button icon="close" label={t("Close dialog")} onClick={close} />
      </header>
      <Show when={error()}>
        <ErrorNotice error={error()} />
      </Show>
      <Switch>
        <Match when={step().kind === "pick"}>
          {/* The filter leads, where typing starts; the account card stays
              under it as one more way in, so filtering moves nothing above. */}
          <input
            ref={search}
            class="connect-search"
            type="search"
            autofocus
            aria-label={t("Search providers")}
            placeholder={t("Search providers…")}
            value={query()}
            onInput={(event) => setQuery(event.currentTarget.value)}
            onKeyDown={(event) => {
              if (event.key !== "Enter") return;
              const first = [...filtered().popular, ...filtered().other][0];
              if (first) {
                event.preventDefault();
                choose(first);
              }
            }}
          />
          <Show when={local()}><AccountMenu models connected={props.close} /></Show>
          <div class="connect-list">
            <Show when={loading() && !catalog().length}>
              <p class="muted connect-empty">{t("Loading…")}</p>
            </Show>
            <For
              each={[
                { label: t("Popular"), rows: filtered().popular },
                { label: t("More providers"), rows: filtered().other },
              ]}
            >
              {(group) => (
                <Show when={group.rows.length}>
                  <section>
                    <span class="section-label">{group.label}</span>
                    <For each={group.rows}>
                      {(provider) => (
                        <button
                          type="button"
                          class="connect-row"
                          classList={{ connected: provider.connected }}
                          aria-label={provider.name}
                          aria-description={status(provider)}
                          data-provider={provider.id}
                          onClick={() => choose(provider)}
                        >
                          <ProviderIcon provider={provider.id} size="lg" />
                          <span class="connect-row-text">
                            <strong>{provider.name}</strong>
                            <small>{status(provider)}</small>
                          </span>
                          <Show when={provider.connected} fallback={<Icon name="chevron" size="sm" />}>
                            <Icon name="check" size="sm" class="connect-check" />
                          </Show>
                        </button>
                      )}
                    </For>
                  </section>
                </Show>
              )}
            </For>
            <Show when={!loading() && !filtered().popular.length && !filtered().other.length}>
              <p class="muted connect-empty">{t("No provider matches that search.")}</p>
            </Show>
            <Show when={local()}>
              <section>
                <span class="section-label">{t("Something else")}</span>
                <button type="button" class="connect-row" onClick={() => setCustom(true)}>
                  <Icon name="cloud" size="lg" />
                  <span class="connect-row-text">
                    <strong>{t("Custom endpoint")}</strong>
                    <small>{t("Any OpenAI-, Anthropic- or Gemini-compatible API")}</small>
                  </span>
                  <Icon name="chevron" size="sm" />
                </button>
                <button type="button" class="connect-row" onClick={() => setRouter(true)}>
                  <Icon name="layers" size="lg" />
                  <span class="connect-row-text">
                    <strong>{t("Haarness Router")}</strong>
                    <small>{t("Self-hosted · one address for every provider")}</small>
                  </span>
                  <Icon name="chevron" size="sm" />
                </button>
              </section>
            </Show>
          </div>
          <footer>
            <button
              type="button"
              class="connect-manage"
              onClick={() => {
                close();
                void app.openSettings("providers");
              }}
            >
              {t("Manage providers in Settings")}
            </button>
          </footer>
        </Match>
        <Match when={step().kind === "method" && step()}>
          {(current) => {
            const provider = () => (current() as Extract<Step, { kind: "method" }>).provider;
            return (
              <div class="connect-methods">
                <For each={usableMethods(provider(), local())}>
                  {(method, index) => (
                    <button
                      type="button"
                      class="connect-method"
                      classList={{ primary: index() === 0 }}
                      disabled={busy()}
                      onClick={() => void start(provider(), method)}
                    >
                      <Icon name={method.kind === "oauth" ? "web" : "key"} size="md" />
                      <span class="connect-row-text">
                        <strong>{t(method.label)}</strong>
                        <small>{method.description}</small>
                      </span>
                    </button>
                  )}
                </For>
                <Show when={provider().env.length}>
                  <EnvironmentHint env={provider().env} />
                </Show>
              </div>
            );
          }}
        </Match>
        <Match when={step().kind === "key" && step()}>
          {(current) => {
            const provider = () => (current() as Extract<Step, { kind: "key" }>).provider;
            return (
              <form class="connect-key" onSubmit={(event) => void submitKey(event)}>
                <label class="field">
                  <span>{t(keyLabel(provider()))}</span>
                  <input
                    ref={keyInput}
                    type="password"
                    autocomplete="off"
                    autofocus
                    aria-label={t(keyLabel(provider()))}
                    placeholder={t("Paste it here")}
                    value={key()}
                    onInput={(event) => setKey(event.currentTarget.value)}
                  />
                </label>
                <p class="muted connect-hint">
                  {local()
                    ? t("Saved on this computer, never in your project.")
                    : t("Saved on this Haarness server, never in your project.")}
                </p>
                <Show when={provider().env.length}>
                  <EnvironmentHint env={provider().env} />
                </Show>
                <footer>
                  <Show when={provider().keyUrl}>
                    {(url) => (
                      <button
                        type="button"
                        class="connect-create-key"
                        onClick={() => void window.haarness.openExternal(url())}
                      >
                        {t("Create a key")}
                        <Icon name="external" size="sm" />
                      </button>
                    )}
                  </Show>
                  <button type="button" disabled={busy()} onClick={back}>
                    {t("Back")}
                  </button>
                  <button type="submit" class="primary" disabled={busy()}>
                    {busy() ? t("Connecting…") : t("Connect")}
                  </button>
                </footer>
              </form>
            );
          }}
        </Match>
        <Match when={step().kind === "login" && step()}>
          {(current) => {
            const login = () => (current() as Extract<Step, { kind: "login" }>).login;
            const provider = () => (current() as Extract<Step, { kind: "login" }>).provider;
            const opened = () => (current() as Extract<Step, { kind: "login" }>).opened;
            return (
              <div class="connect-login" aria-live="polite">
                <Switch>
                  <Match when={login().state === "pending"}>
                    <ol class="connect-steps">
                      <li>
                        <span>
                          {opened()
                            ? t("Your browser opened the sign-in page. If it did not:")
                            : t("Open the sign-in page in your browser:")}
                        </span>
                        <button
                          type="button"
                          class="connect-open"
                          onClick={() => void window.haarness.openExternal(login().url)}
                        >
                          <Icon name="web" size="sm" />
                          {t("Open sign-in page")}
                        </button>
                      </li>
                      <Show when={login().code}>
                        <li>
                          <span>{t("Enter this code when asked:")}</span>
                          <span class="connect-code">
                            <code>{login().code}</code>
                            <Button
                              icon="copy"
                              size="sm"
                              label={t("Copy code")}
                              onClick={() => {
                                void navigator.clipboard.writeText(login().code);
                                app.notify(t("Code copied"));
                              }}
                            />
                          </span>
                        </li>
                      </Show>
                    </ol>
                    <p class="connect-waiting muted">
                      <span class="connect-spinner" aria-hidden="true" />
                      {t("Waiting for you to finish in the browser…")}
                    </p>
                    <footer>
                      <Show when={login().method === "browser" && provider().methods.some((m) => m.id === "device")}>
                        <button
                          type="button"
                          onClick={() => {
                            const device = provider().methods.find((m) => m.id === "device")!;
                            void manage({ action: "login_cancel", id: login().id }).catch(() => {});
                            stopPolling();
                            void start(provider(), device);
                          }}
                        >
                          {t("Use a code instead")}
                        </button>
                      </Show>
                      <button type="button" onClick={back}>
                        {t("Cancel")}
                      </button>
                    </footer>
                  </Match>
                  <Match when={login().state !== "pending"}>
                    <ErrorNotice error={login().error ?? t("Sign-in cancelled")} />
                    <footer>
                      <button type="button" onClick={back}>
                        {t("Back")}
                      </button>
                      <button
                        type="button"
                        class="primary"
                        onClick={() => {
                          const method = provider().methods.find((m) => m.id === login().method);
                          if (method) void start(provider(), method);
                        }}
                      >
                        {t("Try again")}
                      </button>
                    </footer>
                  </Match>
                </Switch>
              </div>
            );
          }}
        </Match>
        <Match when={step().kind === "saved" && step()}>
          {(current) => {
            const saved = () => current() as Extract<Step, { kind: "saved" }>;
            return (
              <div class="connect-saved">
                <div class="notice-inline connect-warning" role="alert">
                  <Icon name="alert" size="sm" />
                  <Show when={saved().warning.reason} fallback={<p>{saved().warning.text}</p>}>
                    <div>
                      <strong>{t("Saved, but the key could not be checked")}</strong>
                      <p>
                        {/* The reason is the server's own words, often English. */}
                        <span dir="auto">{saved().warning.reason}.</span>{" "}
                        {t("If chats then fail to authenticate, replace the key.")}
                      </p>
                    </div>
                  </Show>
                </div>
                <footer>
                  <button type="button" onClick={() => replaceKey(saved().provider)}>
                    {t("Replace key")}
                  </button>
                  <button ref={doneButton} type="button" class="primary" onClick={close}>
                    {t("Done")}
                  </button>
                </footer>
              </div>
            );
          }}
        </Match>
      </Switch>
      <Show when={custom()}>
        <ProviderForm
          close={() => {
            setCustom(false);
            void load();
          }}
        />
      </Show>
      <Show when={router()}>
        <RouterConnectionDialog
          close={() => {
            setRouter(false);
            void load();
          }}
        />
      </Show>
    </dialog>
  );
}

/**
 * Environment variables are another way to connect, not a first-run step, so
 * they wait behind a disclosure instead of sitting in the instructions.
 */
function EnvironmentHint(props: { env: string[] }) {
  return (
    <details class="connect-other">
      <summary>
        <Icon name="chevron" size="xs" />
        {t("Other ways to connect")}
      </summary>
      <p class="muted">
        {splitAround(
          t("Set {env} in the environment before starting Haarness."),
          () => <code>{props.env.join(" / ")}</code>,
          "{env}",
        )}
      </p>
    </details>
  );
}

/** Disconnect with a confirmation, shared by Settings and the picker. */
export async function disconnectProvider(
  app: ReturnType<typeof useApp>,
  provider: { id: string; name: string },
) {
  if (
    !(await confirm(
      t("Disconnect {name}?", { name: provider.name }),
      t("Haarness forgets the stored {name} credential. You can connect it again at any time.", {
        name: provider.name,
      }),
      { submit: t("Disconnect"), danger: true },
    ))
  )
    return false;
  await app.api("manageResource", "providers", { action: "logout", name: provider.id });
  await app.catalogs();
  return true;
}

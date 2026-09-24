import { For, Show, createSignal, onCleanup, onMount } from "solid-js";
import { useApp } from "./state";
import { Button, dismissOnBackdrop, Icon } from "./ui";
import { t } from "./i18n";
import "./account.css";

type Account = {
  loggedIn: boolean;
  user?: { email?: string; displayName?: string };
  accounts?: { id: string; name: string }[];
  selectedAccountId?: string;
};
type CloudSession = {
  id: string;
  title: string;
  agentSnapshot?: { name: string };
  workspace: { observedState: string };
};
type CloudAgent = { id: string; name: string };
export function AccountMenu(props: {
  models?: boolean;
  connected?: () => void;
}) {
  const app = useApp();
  const [account, setAccount] = createSignal<Account>({ loggedIn: false });
  const [open, setOpen] = createSignal(false);
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal("");
  const [loginUrl, setLoginUrl] = createSignal("");
  const [code, setCode] = createSignal("");
  const [workspaces, setWorkspaces] = createSignal<CloudSession[]>([]);
  const [name, setName] = createSignal("");
  const [agents, setAgents] = createSignal<CloudAgent[]>([]);
  const [agentId, setAgentId] = createSignal("");
  const [creating, setCreating] = createSignal(false);
  const [credits, setCredits] = createSignal("");
  let dialog: HTMLDialogElement | undefined;
  let alive = true;
  async function refresh() {
    const result = await window.haarness.account({ action: "status" });
    if (!alive) return;
    setAccount(result);
    if (result.loggedIn && open()) {
      const [spaces, billing, templates] = await Promise.allSettled([
        window.haarness.account({ action: "sessions" }),
        window.haarness.account({ action: "billing" }),
        window.haarness.account({ action: "agents" }),
      ]);
      if (!alive) return;
      if (spaces.status === "fulfilled")
        setWorkspaces(spaces.value.items ?? []);
      else setError(String(spaces.reason));
      if (templates.status === "fulfilled")
        setAgents(templates.value.items ?? []);
      if (billing.status === "fulfilled") {
        const remaining =
          Number(billing.value.includedAi?.credits ?? 0) +
          Number(billing.value.purchased?.credits ?? 0);
        setCredits(
          `${remaining.toLocaleString(undefined, { maximumFractionDigits: 2 })} ${t("credits available")}`,
        );
      }
    } else {
      setWorkspaces([]);
      setCredits("");
    }
  }
  async function act(fn: () => Promise<unknown>) {
    setBusy(true);
    setError("");
    try {
      await fn();
    } catch (e) {
      if (alive) setError(e instanceof Error ? e.message : String(e));
    } finally {
      if (alive) setBusy(false);
    }
  }
  function show() {
    setOpen(true);
    queueMicrotask(() => dialog?.showModal());
    void act(refresh);
  }
  function close() {
    if (busy()) void window.haarness.account({ action: "cancel" });
    dialog?.close();
    setOpen(false);
  }
  async function signIn(device = false) {
    await act(async () => {
      setLoginUrl("");
      setCode("");
      await window.haarness.account({ action: "login", device });
      await refresh();
      if (app.state.connection) await app.catalogs(true);
      if (props.models) {
        close();
        props.connected?.();
      }
    });
  }
  onMount(() => {
    void refresh().catch(() => {});
    const timer = setInterval(() => {
      if (open() && !busy()) void refresh().catch(() => {});
    }, 5000);
    onCleanup(() => clearInterval(timer));
  });
  const off = window.haarness.onEvent((event) => {
    if (event.kind !== "menu") return;
    const data = event.data as { action?: string; url?: string; code?: string };
    if (data.action === "account-login") {
      setLoginUrl(data.url ?? "");
      setCode(data.code ?? "");
    }
    if (data.action === "account-changed") {
      void refresh().catch(() => {});
      if (app.state.connection) void app.catalogs(true).catch(() => {});
    }
  });
  onCleanup(() => {
    alive = false;
    off();
  });
  return (
    <>
      <button
        class={props.models ? "account-connect" : "nav-row"}
        onClick={show}
      >
        <Icon name="cloud" size="md" />
        {props.models
          ? t("Use Haarness Cloud")
          : account().loggedIn
            ? account().user?.displayName || t("Haarness account")
            : t("Sign in to Haarness")}
      </button>
      <Show when={open()}>
        <dialog
          ref={dialog}
          class="dialog account-dialog"
          aria-label={t("Haarness account")}
          {...dismissOnBackdrop(close)}
          onCancel={(event) => {
            event.preventDefault();
            close();
          }}
        >
          <header>
            <div>
              <h2>{t("Haarness account")}</h2>
              <p class="muted">
                {account().loggedIn
                  ? account().user?.email
                  : t("One account for Cloud models and sessions.")}
              </p>
            </div>
            <Button icon="close" label={t("Close dialog")} onClick={close} />
          </header>
          <Show when={error()}>
            <p class="form-error" role="alert">
              {error()}
            </p>
          </Show>
          <Show
            when={account().loggedIn}
            fallback={
              <div class="account-signin">
                <p>
                  {t(
                    "Sign in with your email or Google in your browser. You’ll come back here when you’re done.",
                  )}
                </p>
                <button
                  class="primary"
                  disabled={busy()}
                  onClick={() => void signIn()}
                >
                  {busy()
                    ? t("Waiting for sign-in…")
                    : t("Sign in to Haarness")}
                </button>
                <Show when={loginUrl()}>
                  <button
                    class="secondary"
                    onClick={() =>
                      void window.haarness.openExternal(loginUrl())
                    }
                  >
                    {t("Open sign-in page")}
                  </button>
                </Show>
                <Show when={code()}>
                  <p>
                    {t("Your code")}: <strong>{code()}</strong>
                  </p>
                </Show>
                <Show when={busy()}>
                  <button
                    class="secondary"
                    onClick={() =>
                      void window.haarness.account({ action: "cancel" })
                    }
                  >
                    {t("Cancel")}
                  </button>
                </Show>
              </div>
            }
          >
            <Show when={(account().accounts?.length ?? 0) > 1}>
              <label>
                {t("Account")}
                <select
                  value={account().selectedAccountId}
                  disabled={busy()}
                  onChange={(event) =>
                    void act(async () => {
                      await window.haarness.account({
                        action: "select",
                        id: event.currentTarget.value,
                      });
                      await refresh();
                      if (app.state.connection) await app.catalogs(true);
                    })
                  }
                >
                  <For each={account().accounts}>
                    {(a) => <option value={a.id}>{a.name}</option>}
                  </For>
                </select>
              </label>
            </Show>
            <section>
              <h3>{t("Cloud models")}</h3>
              <p>
                {t(
                  "Choose Haarness Cloud in the model picker to start a conversation.",
                )}
              </p>
              <Show when={credits()}>
                <p>{credits()}</p>
              </Show>
              <div class="account-actions">
                <button
                  class="secondary"
                  onClick={() =>
                    void act(() =>
                      window.haarness.account({
                        action: "web",
                        page: "billing",
                      }),
                    )
                  }
                >
                  {t("Manage billing")}
                </button>
                <button
                  class="secondary"
                  onClick={() =>
                    void act(() =>
                      window.haarness.account({ action: "web", page: "usage" }),
                    )
                  }
                >
                  {t("View usage")}
                </button>
              </div>
            </section>
            <section>
              <div class="account-heading">
                <h3>{t("Cloud sessions")}</h3>
                <button
                  class="secondary"
                  disabled={busy()}
                  onClick={() => setCreating(!creating())}
                >
                  {t("New session")}
                </button>
              </div>
              <Show when={creating()}>
                <form
                  class="account-create"
                  onSubmit={(event) => {
                    event.preventDefault();
                    void act(async () => {
                      await window.haarness.account({
                        action: "create-session",
                        name: name(),
                        id: agentId() || agents()[0]?.id,
                      });
                      setCreating(false);
                      setName("");
                      setAgentId("");
                      await refresh();
                    });
                  }}
                >
                  <input
                    aria-label={t("Session title")}
                    placeholder={t("Session title")}
                    required
                    value={name()}
                    onInput={(event) => setName(event.currentTarget.value)}
                  />
                  <label>
                    {t("Cloud agent")}
                    <select
                      aria-label={t("Cloud agent")}
                      value={agentId() || agents()[0]?.id || ""}
                      onChange={(event) =>
                        setAgentId(event.currentTarget.value)
                      }
                    >
                      <For each={agents()}>
                        {(agent) => (
                          <option value={agent.id}>{agent.name}</option>
                        )}
                      </For>
                    </select>
                  </label>
                  <button
                    type="button"
                    class="secondary"
                    onClick={() =>
                      void window.haarness.account({
                        action: "web",
                        page: "agents",
                      })
                    }
                  >
                    {t("Manage agent templates")}
                  </button>
                  <p class="muted">
                    {t(
                      "Starts an independent session from the selected template. Compute and storage use your credits.",
                    )}
                  </p>
                  <button class="primary" disabled={busy() || !agents().length}>
                    {t("Start session")}
                  </button>
                </form>
              </Show>
              <Show
                when={workspaces().length}
                fallback={<p class="muted">{t("No Cloud sessions yet.")}</p>}
              >
                <For each={workspaces()}>
                  {(w) => (
                    <div class="account-workspace">
                      <div>
                        <strong>{w.title}</strong>
                        <span class="muted">{w.workspace.observedState}</span>
                      </div>
                      <div class="account-actions">
                        <Show
                          when={w.workspace.observedState === "running"}
                          fallback={
                            <button
                              class="secondary"
                              disabled={
                                busy() ||
                                w.workspace.observedState !== "stopped"
                              }
                              onClick={() =>
                                void act(async () => {
                                  await window.haarness.account({
                                    action: "session-start",
                                    id: w.id,
                                  });
                                  await refresh();
                                })
                              }
                            >
                              {t("Start")}
                            </button>
                          }
                        >
                          <button
                            class="primary"
                            disabled={busy()}
                            onClick={() =>
                              void act(async () => {
                                const connected = await app.connect({
                                  cloudSession: w.id,
                                });
                                if (connected) close();
                              })
                            }
                          >
                            {t("Open")}
                          </button>
                          <button
                            class="secondary"
                            disabled={busy()}
                            onClick={() =>
                              void act(async () => {
                                await window.haarness.account({
                                  action: "session-stop",
                                  id: w.id,
                                });
                                await refresh();
                              })
                            }
                          >
                            {t("Stop")}
                          </button>
                        </Show>
                      </div>
                    </div>
                  )}
                </For>
              </Show>
              <button
                class="secondary"
                disabled={busy()}
                onClick={() => void act(refresh)}
              >
                {t("Refresh")}
              </button>
            </section>
            <footer>
              <button
                class="secondary"
                disabled={busy()}
                onClick={() =>
                  void act(async () => {
                    await window.haarness.account({ action: "logout" });
                    await refresh();
                    if (app.state.connection) await app.catalogs(true);
                  })
                }
              >
                {t("Sign out")}
              </button>
              <button class="primary" onClick={close}>
                {t("Done")}
              </button>
            </footer>
          </Show>
        </dialog>
      </Show>
    </>
  );
}

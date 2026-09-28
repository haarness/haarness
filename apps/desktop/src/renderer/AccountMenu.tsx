import { For, Show, createSignal, onCleanup, onMount } from "solid-js";
import { useApp } from "./state";
import { Button, dismissOnBackdrop, Icon } from "./ui";
import { t } from "./i18n";
import "./account.css";
import { credits, type BillingState } from "../billing";

type Account = {
  loggedIn: boolean;
  authKind?: string;
  scopes?: string[];
  user?: { email?: string; displayName?: string };
  accounts?: { id: string; name: string }[];
  selectedAccountId?: string;
  selectedRouterId?: string;
};
type CloudSession = {
  id: string;
  title: string;
  agentSnapshot?: { name: string };
  workspace: { observedState: string };
};
type CloudRouter = { id: string; name: string; isDefault: boolean; credentialMode: string };
type CloudAgent = { id: string; name: string };
export function AccountMenu(props: {
  models?: boolean;
  connected?: () => void;
}) {
  const app = useApp();
  const [account, setAccount] = createSignal<Account>({ loggedIn: false });
  const permits = (scope: string) => account().authKind !== "api_key" || account().scopes?.includes(scope);
  const [open, setOpen] = createSignal(false);
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal("");
  const [loginUrl, setLoginUrl] = createSignal("");
  const [code, setCode] = createSignal("");
  const [workspaces, setWorkspaces] = createSignal<CloudSession[]>([]);
  const [name, setName] = createSignal("");
  const [routers, setRouters] = createSignal<CloudRouter[]>([]);
  const [agents, setAgents] = createSignal<CloudAgent[]>([]);
  const [agentId, setAgentId] = createSignal("");
  const [configuration, setConfiguration] = createSignal("");
  const [instructions, setInstructions] = createSignal("");
  const [creating, setCreating] = createSignal(false);
  const [billing, setBilling] = createSignal<BillingState>({ loggedIn: false });
  const summary = () => billing().summary?.accountId === account().selectedAccountId ? billing().summary : undefined;
  const date = (value: string) => new Date(value).toLocaleDateString();
  let dialog: HTMLDialogElement | undefined;
  let alive = true;
  async function refresh() {
    const result = await window.haarness.account({ action: "status" });
    if (!alive) return;
    setAccount(result);
    if (result.loggedIn && open() && (result.authKind !== "api_key" || result.scopes?.includes("models:read"))) {
      try {
        const catalog = await window.haarness.account({ action: "routers" });
        if (!alive) return;
        setRouters(catalog.items ?? []);
      } catch (cause) {
        if (!alive) return;
        setRouters([]); setError(String(cause));
      }
    } else setRouters([]);
    if (result.loggedIn && open() && (result.authKind !== "api_key" || result.scopes?.includes("workspaces:read"))) {
      const [spaces, templates] = await Promise.allSettled([
        window.haarness.account({ action: "sessions" }),
        window.haarness.account({ action: "agents" }),
      ]);
      if (!alive) return;
      if (spaces.status === "fulfilled")
        setWorkspaces(spaces.value.items ?? []);
      else setError(String(spaces.reason));
      if (templates.status === "fulfilled")
        setAgents(templates.value.items ?? []);

    } else {
      setWorkspaces([]);
      if (!result.loggedIn) setBilling({ loggedIn: false });
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
    void window.haarness.account({ action: "billing-state" }).then(value => { if (alive) setBilling(value); });
    const timer = setInterval(() => {
      if (open() && !busy()) void refresh().catch(() => {});
    }, 5000);
    onCleanup(() => clearInterval(timer));
  });
  const off = window.haarness.onEvent((event) => {
    if (event.kind !== "menu") return;
    const data = event.data as { action?: string; url?: string; code?: string; state?: BillingState };
    if (data.action === "account-billing" && data.state) setBilling(data.state);
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
        aria-label={props.models ? t("Use Haarness Cloud") : account().loggedIn ? account().user?.displayName || t("Haarness account") : t("Sign in to Haarness")}
        onClick={show}
      >
        <Icon name="cloud" size="md" />
        <span class="account-nav-label">{props.models
          ? t("Use Haarness Cloud")
          : account().loggedIn
            ? account().user?.displayName || t("Haarness account")
            : t("Sign in to Haarness")}
        <Show when={!props.models && summary()}>{s => <small>{s().plan} · {billing().error ? t("Billing unavailable") : `${credits(s().includedCredits)} + ${credits(s().purchasedCredits)} ${t("credits")}`}</small>}</Show>
        </span>
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
            <Show when={routers().length}>
              <label>{t("Cloud router")}
                <select aria-label={t("Cloud router")} value={account().selectedRouterId ?? routers().find(r => r.isDefault)?.id} disabled={busy()}
                  onChange={event => { const id = event.currentTarget.value; void act(async () => {
                    await window.haarness.account({ action: "select-router", id });
                    await refresh();
                    if (app.state.connection) await app.catalogs(true);
                  }); }}>
                  <For each={routers()}>{router => <option value={router.id}>{router.name} · {router.credentialMode === "byok" ? t("Your keys — no Cloud credits") : t("Cloud credits")}</option>}</For>
                </select>
              </label>
              <p class="muted">{t("Applies to future Haarness Cloud model requests on this computer. Manage routers and provider keys in Cloud.")}</p>
            </Show>
            <Show when={account().authKind === "api_key"}><p class="muted">{t("Connected with HAARNESS_API_KEY. Manage its permissions and expiration in Cloud. Unset the environment variable to disconnect.")}</p></Show>
            <Show when={permits("billing:read")} fallback={<p class="muted">{t("This API key does not include billing access.")}</p>}>
            <section class="account-billing" aria-label={t("Plan and credits")}>
              <div class="account-heading"><h3>{t("Plan and credits")}</h3><Show when={summary()}>{s => <span class="account-plan">{s().plan}</span>}</Show></div>
              <Show when={billing().error}><p class="form-error" role="status">{billing().error} {summary() ? t("Showing last known balances.") : ""}</p></Show>
              <Show when={summary()} fallback={<Show when={!billing().error}><p class="muted">{t("Loading billing…")}</p></Show>}>{s => <>
                <p class="muted">{s().accountName} · {s().subscriptionStatus ?? t("No subscription")}<Show when={s().accountStatus !== "active"}> · {s().accountStatus}</Show></p>
                <dl class="account-balances">
                  <div><dt>{t("Included AI credits")}</dt><dd data-testid="included-credits">{credits(s().includedCredits)}</dd></div>
                  <div><dt>{t("Purchased credits")}</dt><dd data-testid="purchased-credits">{credits(s().purchasedCredits)}</dd></div>
                  <div><dt>{t("Reserved credits")}</dt><dd>{credits(s().heldCredits)}</dd></div>
                  <div><dt>{t("Cloud units available")}</dt><dd>{s().cloudUnits}</dd></div>
                </dl>
                <p class="muted">{t("Available balances exclude reserved credits.")} {t("Paid overage")}: {s().overageOptIn ? t("Enabled") : t("Disabled")}.</p>
                <Show when={s().resetsAt}>{at => <p class="muted">{t("Allowance resets")}: {date(at())}</p>}</Show>
                <Show when={s().periodEndsAt}>{at => <p class="muted">{s().cancelAtPeriodEnd ? t("Plan ends") : t("Plan renews")}: {date(at())}</p>}</Show>
              </>}</Show>
              <div class="account-actions">
                <button class="primary" onClick={() => void act(() => window.haarness.account({ action: "web", page: "topup" }))}>{t("Add credits")}</button>
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
            </Show>
            <Show when={permits("workspaces:read")} fallback={<p class="muted">{t("This API key does not include Cloud session access.")}</p>}>
            <section>
              <div class="account-heading">
                <h3>{t("Cloud sessions")}</h3>
                <button
                  class="secondary"
                  disabled={busy() || !permits("workspaces:write")}
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
                        id: agentId() || undefined,
                        configuration: { ...(configuration().trim() ? JSON.parse(configuration()) : {}), ...(instructions().trim() ? { instructions: instructions() } : {}) },
                      });
                      setCreating(false);
                      setName("");
                      setAgentId("");
                      setConfiguration("");
                      setInstructions("");
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
                    {t("Use Cloud agent (optional)")}
                    <select
                      aria-label={t("Use Cloud agent (optional)")}
                      value={agentId()}
                      onChange={(event) =>
                        setAgentId(event.currentTarget.value)
                      }
                    >
                      <option value="">{t("No template")}</option>
                      <For each={agents()}>
                        {(agent) => (
                          <option value={agent.id}>{agent.name}</option>
                        )}
                      </For>
                    </select>
                  </label>
                  <label>{t("Instructions")}<textarea aria-label={t("Session instructions")} value={instructions()} onInput={e => setInstructions(e.currentTarget.value)} placeholder={t("Optional instructions for this session")} /></label>
                  <details><summary>{t("Advanced session configuration")}</summary><p class="muted">{t("Optional JSON settings for model, reasoningEffort, mcp, agents, skills and permissions. Supplied fields override the template. Manage secrets in Cloud session settings.")}</p><textarea aria-label={t("Session configuration JSON")} rows={6} value={configuration()} onInput={e => setConfiguration(e.currentTarget.value)} placeholder={'{"mcp":{},"agents":{},"skills":{}}'} /></details>
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
                      "Starts an independent session. A template is optional. Compute and storage use your credits.",
                    )}
                  </p>
                  <button class="primary" disabled={busy()}>
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
                        <button class="secondary" onClick={() => void window.haarness.account({action:"web", page:"session-settings", id:w.id})}>{t("Settings")}</button>
                        <Show
                          when={w.workspace.observedState === "running"}
                          fallback={
                            <button
                              class="secondary"
                              disabled={
                                busy() || !permits("workspaces:write") ||
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
                            disabled={busy() || !permits("workspaces:agent")}
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
                            disabled={busy() || !permits("workspaces:write")}
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
            </Show>
            <footer>
              <Show when={account().authKind !== "api_key"}><button
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
              </button></Show>
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

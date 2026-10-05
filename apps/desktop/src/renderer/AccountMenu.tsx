import { For, Show, createSignal, onCleanup, onMount } from "solid-js";
import { useApp } from "./state";
import { Button, dismissOnBackdrop, Icon } from "./ui";
import { Select } from "./Select";
import { Shimmer } from "./Shimmer";
import { language, t } from "./i18n";
import "./account.css";
import {
  addCredits,
  credits,
  creditsShort,
  planName,
  type BillingState,
} from "../billing";

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

// English keys; t() runs where they render.
const sessionStates: Record<string, string> = {
  created: "Preparing…",
  pending: "Preparing…",
  starting: "Starting…",
  running: "Running",
  stopping: "Stopping…",
  stopped: "Stopped",
  failed: "Failed",
  deleting: "Deleting…",
};
const subscriptionStates: Record<string, string> = {
  active: "Active",
  trialing: "Trial",
  past_due: "Payment due",
  unpaid: "Payment due",
  canceled: "Canceled",
  incomplete: "Incomplete",
  paused: "Paused",
};
const initials = (name: string) =>
  name
    .split(/[\s@._-]+/)
    .filter(Boolean)
    .slice(0, 2)
    .map((part) => part[0]!.toUpperCase())
    .join("") || "H";

export function AccountMenu(props: {
  models?: boolean;
  connected?: () => void;
}) {
  const app = useApp();
  const [account, setAccount] = createSignal<Account>({ loggedIn: false });
  const permits = (scope: string) => account().authKind !== "api_key" || account().scopes?.includes(scope);
  const [open, setOpen] = createSignal(false);
  const [busy, setBusy] = createSignal(false);
  const [signingIn, setSigningIn] = createSignal(false);
  const [error, setError] = createSignal("");
  const [loginUrl, setLoginUrl] = createSignal("");
  const [code, setCode] = createSignal("");
  const [copied, setCopied] = createSignal(false);
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
  const date = (value: string) =>
    new Intl.DateTimeFormat(language(), { month: "short", day: "numeric", year: "numeric" }).format(new Date(value));
  const hours = (value: string) =>
    Number(value).toLocaleString(language(), { maximumFractionDigits: 1 });
  const displayName = () =>
    account().user?.displayName || account().user?.email || t("Haarness account");
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
    if (signingIn()) cancelSignIn();
    dialog?.close();
    setOpen(false);
  }
  // A cancel the user asked for is not a failure worth an alert.
  let cancelled = false;
  function cancelSignIn() {
    cancelled = true;
    void window.haarness.account({ action: "cancel" });
  }
  async function signIn(device = false) {
    cancelled = false;
    setSigningIn(true);
    await act(async () => {
      setLoginUrl("");
      setCode("");
      try {
        await window.haarness.account({ action: "login", device });
      } catch (cause) {
        if (cancelled) return;
        throw cause;
      }
      await refresh();
      if (app.state.connection) await app.catalogs(true);
      if (props.models) {
        close();
        props.connected?.();
      }
    });
    if (alive) setSigningIn(false);
  }
  async function copyLink() {
    await navigator.clipboard.writeText(loginUrl());
    setCopied(true);
    setTimeout(() => alive && setCopied(false), 1600);
  }
  const web = (page: string, id?: string) =>
    act(() => window.haarness.account({ action: "web", page, ...(id ? { id } : {}) }));
  onMount(() => {
    // Only the sidebar's instance answers app-wide requests to show the account.
    if (!props.models) {
      const request = () => show();
      window.addEventListener("haarness:account", request);
      onCleanup(() => window.removeEventListener("haarness:account", request));
    }
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
  // The trial is not a plan name; it reads as "Free trial" in the reader's language.
  const planLabel = (s: NonNullable<BillingState["summary"]>) => (s.trial ? t("Free trial") : planName(s.plan));
  const navSummary = () => {
    const s = summary();
    if (!s) return undefined;
    if (billing().error) return t("Billing unavailable");
    const total = creditsShort(addCredits(s.includedCredits, s.purchasedCredits));
    return t("{plan} · {credits} credits", { plan: planLabel(s) ?? t("No plan"), credits: total });
  };
  return (
    <>
      <button
        class={props.models ? "account-connect" : "nav-row"}
        aria-label={props.models ? t("Use Haarness Cloud") : account().loggedIn ? account().user?.displayName || t("Haarness account") : t("Sign in to Haarness")}
        onClick={show}
      >
        <Show
          when={props.models}
          fallback={
            <>
              <Icon name="cloud" size="md" />
              <span class="account-nav-label">
                {account().loggedIn
                  ? account().user?.displayName || t("Haarness account")
                  : t("Sign in to Haarness")}
                <Show when={navSummary()}>{text => <small>{text()}</small>}</Show>
              </span>
            </>
          }
        >
          <span class="account-connect-icon" aria-hidden="true">
            <Icon name="cloud" size="lg" />
          </span>
          <span class="account-connect-text">
            <strong>{t("Use Haarness Cloud")}</strong>
            <small>{t("Top models with one Haarness account. No API keys to manage.")}</small>
          </span>
          <Icon name="chevron" size="sm" />
        </Show>
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
          <header class="account-header">
            <Show
              when={account().loggedIn}
              fallback={
                <span class="account-avatar account-avatar-brand" aria-hidden="true">
                  <Icon name="cloud" size="lg" />
                </span>
              }
            >
              <span class="account-avatar" aria-hidden="true">{initials(displayName())}</span>
            </Show>
            <div class="account-identity">
              <h2>{account().loggedIn ? displayName() : t("Haarness account")}</h2>
              <p class="muted">
                {account().loggedIn
                  ? account().user?.email
                  : t("One account for Cloud models, sessions and billing.")}
              </p>
            </div>
            <Button icon="close" label={t("Close dialog")} onClick={close} />
          </header>
          <div class="account-body">
            <Show when={error()}>
              <p class="form-error" role="alert">
                {error()}
              </p>
            </Show>
            <Show
              when={account().loggedIn}
              fallback={
                <div class="account-signin">
                  <Show
                    when={signingIn()}
                    fallback={
                      <>
                        <ul class="account-perks">
                          <li>
                            <Icon name="reasoning" size="md" />
                            <span>{t("Use Cloud models without managing provider keys.")}</span>
                          </li>
                          <li>
                            <Icon name="cloud" size="md" />
                            <span>{t("Run Cloud sessions that keep working when your computer sleeps.")}</span>
                          </li>
                          <li>
                            <Icon name="billing" size="md" />
                            <span>{t("Share one plan and credit balance across Desktop, the CLI and the web.")}</span>
                          </li>
                        </ul>
                        <button class="primary account-signin-button" onClick={() => void signIn()}>
                          {t("Sign in to Haarness")}
                        </button>
                        <p class="account-hint">
                          {t("Opens your browser. You’ll come back here when you’re done.")}
                        </p>
                      </>
                    }
                  >
                    <div class="account-waiting" role="status">
                      <Shimmer class="account-waiting-label" active>
                        {t("Waiting for you to finish in your browser…")}
                      </Shimmer>
                      <Show when={code()}>
                        <p class="account-code">
                          <span class="muted">{t("Your code")}</span>
                          <strong>{code()}</strong>
                        </p>
                      </Show>
                      <div class="account-waiting-actions">
                        <Show when={loginUrl()}>
                          <button onClick={() => void window.haarness.openExternal(loginUrl())}>
                            <Icon name="external" size="sm" />
                            {t("Open sign-in page")}
                          </button>
                          <button onClick={() => void copyLink()}>
                            <Icon name={copied() ? "check" : "copy"} size="sm" />
                            {copied() ? t("Copied") : t("Copy link")}
                          </button>
                        </Show>
                        <button
                          class="account-waiting-cancel"
                          onClick={cancelSignIn}
                        >
                          {t("Cancel")}
                        </button>
                      </div>
                    </div>
                  </Show>
                </div>
              }
            >
              <Show when={(account().accounts?.length ?? 0) > 1}>
                <div class="account-row">
                  <div>
                    <strong>{t("Account")}</strong>
                    <p class="muted">{t("Billing, sessions and members belong to the selected account.")}</p>
                  </div>
                  <Select
                    aria-label={t("Account")}
                    value={account().selectedAccountId}
                    disabled={busy()}
                    options={(account().accounts ?? []).map((a) => ({ value: a.id, label: a.name }))}
                    onChange={(id) =>
                      void act(async () => {
                        await window.haarness.account({ action: "select", id });
                        await refresh();
                        if (app.state.connection) await app.catalogs(true);
                      })
                    }
                  />
                </div>
              </Show>
              <Show when={account().authKind === "api_key"}><p class="account-note">{t("Connected with HAARNESS_API_KEY. Manage its permissions and expiration in Cloud. Unset the environment variable to disconnect.")}</p></Show>
              <Show when={permits("billing:read")} fallback={<p class="account-note">{t("This API key does not include billing access.")}</p>}>
              <section class="account-section account-billing" aria-label={t("Plan and credits")}>
                <div class="account-heading">
                  <h3>{t("Plan and credits")}</h3>
                  <Show when={summary()}>
                    {s => (
                      <span class="account-plan" data-plan={planName(s().plan) ? "paid" : "none"}>
                        {planLabel(s()) ?? t("No plan")}
                        <Show when={s().billingInterval === "year"}>{" · " + t("Yearly")}</Show>
                        <Show when={planName(s().plan) && s().subscriptionStatus && s().subscriptionStatus !== "active"}>
                          {" · " + t(subscriptionStates[s().subscriptionStatus!] ?? s().subscriptionStatus!)}
                        </Show>
                      </span>
                    )}
                  </Show>
                </div>
                <Show when={billing().error}><p class="form-error" role="status">{billing().error} {summary() ? t("Showing last known balances.") : ""}</p></Show>
                <Show when={summary()} fallback={<Show when={!billing().error}><p class="muted">{t("Loading billing…")}</p></Show>}>{s => <>
                  <Show when={!planName(s().plan)}>
                    <div class="account-upsell">
                      <p>{t("Choose a plan for monthly AI credits and Cloud compute, or add credits that never expire.")}</p>
                      <Show when={s().trialAvailable}>
                        <button class="primary" data-testid="start-trial" onClick={() => void web("billing")}>
                          {t("Start free trial")}
                          <Icon name="external" size="sm" />
                        </button>
                      </Show>
                      <button classList={{ primary: !s().trialAvailable }} onClick={() => void web("billing")}>
                        {t("See plans")}
                        <Icon name="external" size="sm" />
                      </button>
                    </div>
                  </Show>
                  <Show when={s().outOfCredits}>
                    <p class="form-error" role="status" data-testid="out-of-credits">{t("You're out of AI credits.")}</p>
                  </Show>
                  <Show when={s().accountStatus !== "active"}>
                    <p class="form-error" role="status">{t("This account is {status}. Cloud requests are paused until billing is resolved.", { status: s().accountStatus })}</p>
                  </Show>
                  <dl class="account-balances">
                    <div>
                      <dt>{t("Included credits")}</dt>
                      <dd data-testid="included-credits">{credits(s().includedCredits)}</dd>
                      <span>
                        {s().resetsAt ? t("Resets {date}", { date: date(s().resetsAt!) }) : t("From your plan")}
                      </span>
                    </div>
                    <div>
                      <dt>{t("Purchased credits")}</dt>
                      <dd data-testid="purchased-credits">{credits(s().purchasedCredits)}</dd>
                      <span>{t("Never expire")}</span>
                    </div>
                    <div>
                      <dt>{t("Cloud compute")}</dt>
                      <dd>{t("{hours} h", { hours: hours(s().cloudUnits) })}</dd>
                      <span>{t("Left this period")}</span>
                    </div>
                  </dl>
                  <Show when={/[1-9]/.test(s().heldCredits)}>
                    <p class="account-note">{t("{credits} credits are held by requests in progress.", { credits: credits(s().heldCredits) })}</p>
                  </Show>
                  <Show when={!s().outOfCredits && s().daysLeft != null && s().daysLeft! < 30}>
                    <p class="account-note" data-testid="days-left">{t("About {days} days left at this pace", { days: s().daysLeft! })}</p>
                  </Show>
                  <Show when={s().autoTopupFailure}>
                    {reason => <p class="form-error" role="status">{t("The last automatic top-up failed, so they are off: {reason}", { reason: reason() })}</p>}
                  </Show>
                  <Show when={s().periodEndsAt}>
                    {at => (
                      <p class="account-note">
                        {s().cancelAtPeriodEnd
                          ? t("Your plan ends on {date}.", { date: date(at()) })
                          : t("Your plan renews on {date}.", { date: date(at()) })}
                        <Show when={s().overageOptIn}>{" " + t("Overage is on: purchased credits cover usage beyond your allowance.")}</Show>
                      </p>
                    )}
                  </Show>
                </>}</Show>
                <div class="account-actions">
                  <button class="primary" onClick={() => void web("topup")}>{t("Add credits")}</button>
                  <button onClick={() => void web("billing")}>
                    {t("Manage billing")}
                    <Icon name="external" size="sm" />
                  </button>
                  <button onClick={() => void web("usage")}>
                    {t("View usage")}
                    <Icon name="external" size="sm" />
                  </button>
                </div>
              </section>
              </Show>
              <Show when={routers().length > 1}>
                <section class="account-section">
                  <div class="account-row">
                    <div>
                      <strong>{t("Model router")}</strong>
                      <p class="muted">{t("Haarness Cloud model requests from this computer go through this router.")}</p>
                    </div>
                    <Select
                      aria-label={t("Cloud router")}
                      value={account().selectedRouterId ?? routers().find(r => r.isDefault)?.id}
                      disabled={busy()}
                      options={routers().map(router => ({
                        value: router.id,
                        label: router.name,
                        group: router.credentialMode === "byok" ? t("Your provider keys · no Cloud credits") : t("Cloud credits"),
                      }))}
                      onChange={id => void act(async () => {
                        await window.haarness.account({ action: "select-router", id });
                        await refresh();
                        if (app.state.connection) await app.catalogs(true);
                      })}
                    />
                  </div>
                </section>
              </Show>
              <Show when={permits("workspaces:read")} fallback={<p class="account-note">{t("This API key does not include Cloud session access.")}</p>}>
              <section class="account-section">
                <div class="account-heading">
                  <h3>{t("Cloud sessions")}</h3>
                  <Show when={!creating()}>
                    <button
                      disabled={busy() || !permits("workspaces:write")}
                      onClick={() => setCreating(true)}
                    >
                      <Icon name="plus" size="sm" />
                      {t("New session")}
                    </button>
                  </Show>
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
                    <label class="field">
                      <span>{t("Title")}</span>
                      <input
                        aria-label={t("Session title")}
                        placeholder={t("What will this session work on?")}
                        required
                        value={name()}
                        onInput={(event) => setName(event.currentTarget.value)}
                      />
                    </label>
                    <div class="field">
                      <span>{t("Cloud agent")} <em>{t("Optional")}</em></span>
                      <Select
                        aria-label={t("Use Cloud agent (optional)")}
                        value={agentId()}
                        options={[
                          { value: "", label: t("No template") },
                          ...agents().map((agent) => ({ value: agent.id, label: agent.name })),
                        ]}
                        onChange={setAgentId}
                      />
                    </div>
                    <label class="field">
                      <span>{t("Instructions")} <em>{t("Optional")}</em></span>
                      <textarea
                        aria-label={t("Session instructions")}
                        rows={3}
                        value={instructions()}
                        onInput={e => setInstructions(e.currentTarget.value)}
                        placeholder={t("For example: run the test suite and summarise any failures.")}
                      />
                    </label>
                    <details class="account-advanced">
                      <summary>
                        <Icon name="chevron" size="xs" />
                        {t("Advanced session configuration")}
                      </summary>
                      <p class="muted">{t("Optional JSON settings for model, reasoningEffort, mcp, agents, skills and permissions. Supplied fields override the template. Manage secrets in Cloud session settings.")}</p>
                      <textarea aria-label={t("Session configuration JSON")} rows={6} value={configuration()} onInput={e => setConfiguration(e.currentTarget.value)} placeholder={'{"mcp":{},"agents":{},"skills":{}}'} />
                    </details>
                    <p class="account-hint">
                      {t("Starts an independent Cloud machine with its own files. It runs on your plan's compute hours and disk.")}
                    </p>
                    <div class="account-create-footer">
                      <button type="button" class="account-link" onClick={() => void web("agents")}>
                        {t("Manage agent templates")}
                        <Icon name="external" size="sm" />
                      </button>
                      <button type="button" onClick={() => setCreating(false)}>
                        {t("Cancel")}
                      </button>
                      <button class="primary" disabled={busy()}>
                        {t("Start session")}
                      </button>
                    </div>
                  </form>
                </Show>
                <Show
                  when={workspaces().length}
                  fallback={
                    <Show when={!creating()}>
                      <p class="account-empty">
                        {t("No Cloud sessions yet. A session runs Haarness on a Cloud machine with its own files, and keeps working while you are away.")}
                      </p>
                    </Show>
                  }
                >
                  <ul class="account-sessions">
                    <For each={workspaces()}>
                      {(w) => (
                        <li class="account-workspace" data-state={w.workspace.observedState}>
                          <span class="account-session-dot" aria-hidden="true" />
                          <div class="account-session-text">
                            <strong>{w.title}</strong>
                            <Shimmer
                              class="account-session-state"
                              active={/…$/.test(sessionStates[w.workspace.observedState] ?? "")}
                            >
                              {t(sessionStates[w.workspace.observedState] ?? w.workspace.observedState)}
                              <Show when={w.agentSnapshot?.name}>{" · " + w.agentSnapshot!.name}</Show>
                            </Shimmer>
                          </div>
                          <div class="account-session-actions">
                            <Show when={w.workspace.observedState === "running"}>
                              <button
                                disabled={busy() || !permits("workspaces:write")}
                                onClick={() =>
                                  void act(async () => {
                                    await window.haarness.account({ action: "session-stop", id: w.id });
                                    await refresh();
                                  })
                                }
                              >
                                {t("Stop")}
                              </button>
                              <button
                                class="primary"
                                disabled={busy() || !permits("workspaces:agent")}
                                onClick={() =>
                                  void act(async () => {
                                    const connected = await app.connect({ cloudSession: w.id });
                                    if (connected) close();
                                  })
                                }
                              >
                                {t("Open")}
                              </button>
                            </Show>
                            <Show when={w.workspace.observedState === "stopped"}>
                              <button
                                disabled={busy() || !permits("workspaces:write")}
                                onClick={() =>
                                  void act(async () => {
                                    await window.haarness.account({ action: "session-start", id: w.id });
                                    await refresh();
                                  })
                                }
                              >
                                <Icon name="start" size="sm" />
                                {t("Start")}
                              </button>
                            </Show>
                            <Button
                              icon="settings"
                              label={t("Cloud session settings")}
                              onClick={() => void web("session-settings", w.id)}
                            />
                          </div>
                        </li>
                      )}
                    </For>
                  </ul>
                </Show>
              </section>
              </Show>
            </Show>
          </div>
          <Show when={account().loggedIn}>
            <footer class="account-footer">
              <Show when={account().authKind !== "api_key"}>
                <button
                  class="account-signout"
                  disabled={busy()}
                  onClick={() =>
                    void act(async () => {
                      await window.haarness.account({ action: "logout" });
                      await refresh();
                      if (app.state.connection) await app.catalogs(true);
                    })
                  }
                >
                  <Icon name="sign-out" size="sm" />
                  {t("Sign out")}
                </button>
              </Show>
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

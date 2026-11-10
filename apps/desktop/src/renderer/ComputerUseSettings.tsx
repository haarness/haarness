import { For, Show, createMemo, createSignal, onCleanup } from "solid-js";
import { useApp } from "./state";
import { Select } from "./Select";
import { SettingRow as Row } from "./SettingRow";
import { Icon } from "./ui";
import { errorLine, language, t } from "./i18n";
import { cancelled } from "./error-text";
import {
  COMPUTER_USE_CAPABILITY,
  barWords,
  defaultChoices,
  ruleChoices,
  validBundle,
  type ComputerRule,
  type ComputerUseStatus,
} from "./computer-use";
import "./computer-use.css";

/**
 * The Computer use settings pane: the on/off switch and its terms, the macOS
 * permission state, and per-app rules. These settings are machine-wide on the
 * server's Mac, so the pane only works against this computer's own server.
 * Permissions are granted in System Settings, outside Desktop, which is why
 * the pane refreshes itself every three seconds when shown.
 */
export function ComputerUseSettings() {
  const app = useApp();
  const [status, setStatus] = createSignal<ComputerUseStatus>();
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal("");
  const [agreed, setAgreed] = createSignal(false);
  const [showTerms, setShowTerms] = createSignal(false);
  const [bundle, setBundle] = createSignal("");
  const [rule, setRule] = createSignal<ComputerRule>("allow");
  const [invalid, setInvalid] = createSignal(false);
  // macOS shows its prompts once, for both permissions; after that a click
  // opens the permission's own place in System Settings.
  const [asked, setAsked] = createSignal(false);
  const local = () => !!app.state.connection && !app.state.connection.remote;
  const capable = createMemo(() => !!app.state.connection?.info.capabilities.includes(COMPUTER_USE_CAPABILITY));
  let generation = 0;
  let polling = false;
  let shown = "";

  async function request(input: Record<string, unknown> = { action: "get" }, background = false) {
    const connection = app.state.connection?.id;
    if (!connection || !capable() || !local()) return;
    if (background ? polling || busy() : busy()) return;
    const version = ++generation;
    if (background) polling = true;
    else setBusy(true);
    try {
      const result = await window.haarness.api<{ data: ComputerUseStatus }>(connection, "manageResource", [
        "computer-use",
        { ...input, bar: barWords((key) => t(key)) },
      ]);
      if (version !== generation || connection !== app.state.connection?.id) return;
      // A refresh that changes nothing leaves every row and control as it is.
      const text = JSON.stringify(result.data);
      if (text !== shown) {
        // Agreeing again after a withdrawal (here, in the terminal or in
        // another window) takes a new tick: the old one is not carried over.
        if (status()?.terms.accepted !== result.data.terms.accepted) setAgreed(false);
        shown = text;
        setStatus(result.data);
      }
      setError("");
    } catch (cause) {
      if (version === generation && !cancelled(cause)) setError(errorLine(cause));
    } finally {
      if (background) polling = false;
      else if (version === generation) setBusy(false);
    }
  }
  void request();
  const timer = setInterval(() => void request({ action: "get" }, true), 3000);
  onCleanup(() => {
    generation++;
    clearInterval(timer);
  });

  const host = () => {
    const value = status()?.host;
    return value?.haarness ? "Haarness" : (value?.name ?? value?.bundle ?? "");
  };
  const allowPermission = (pane: "accessibility" | "screen-recording") => {
    const first = !asked();
    setAsked(true);
    void request(first ? { action: "permissions" } : { action: "open", pane });
  };
  const addRule = (event: Event) => {
    event.preventDefault();
    const value = bundle().trim();
    if (!validBundle(value)) return setInvalid(true);
    setInvalid(false);
    setBundle("");
    void request({ action: "app", bundle: value, rule: rule() });
  };
  const agreedOn = () => {
    const at = status()?.terms.acceptedAt;
    return at ? new Date(at).toLocaleDateString(language(), { dateStyle: "long" }) : "";
  };
  const permission = (granted: boolean | undefined, pane: "accessibility" | "screen-recording", name: string) => (
    <>
      <span class="computer-permission" classList={{ granted: !!granted }}>
        <Show when={granted} fallback={<span class="computer-permission-dot" aria-hidden="true" />}>
          <Icon name="check" size="xs" />
        </Show>
        {granted ? t("Allowed") : t("Not allowed")}
      </span>
      <Show when={!granted}>
        <button
          type="button"
          disabled={busy()}
          aria-label={t("Allow {permission} in System Settings", { permission: name })}
          onClick={() => allowPermission(pane)}
        >
          {t("Allow…")}
        </button>
      </Show>
    </>
  );

  return (
    <div class="computer-use-settings">
      <p class="muted">
        {t("Agents get to view this Mac's screen and operate its apps. They look at one app at a time and check with you before acting in an app you haven't allowed.")}
      </p>
      <Show when={error()}>
        <p class="notice-inline" role="alert">
          {error()}
        </p>
      </Show>
      <Show
        when={local() && capable()}
        fallback={
          <p class="muted computer-quiet" data-testid="computer-use-unavailable">
            {!local()
              ? t("Computer use is set on the Mac a server runs on. Open a project on this computer to change it.")
              : t("This Haarness server has no computer use. Update Haarness to use it.")}
          </p>
        }
      >
        <Show when={status()} fallback={<p class="muted computer-quiet">{t("Loading…")}</p>}>
          {(current) => (
            <Show
              when={current().supported}
              fallback={
                <p class="muted computer-quiet" data-testid="computer-use-unavailable">
                  {t("Computer use needs macOS and a build of Haarness that includes it.")}
                </p>
              }
            >
              <Show when={current().terms.accepted} fallback={<Agreement status={current()} busy={busy()} agreed={agreed()} setAgreed={setAgreed} agree={() => void request({ action: "accept", version: current().terms.version })} />}>
                <Row
                  title="Agents may use this Mac"
                  description="While an agent works, a strip along the top edge of the screen shows the app in use, with Pause and Stop. Esc also stops it while it works in the app in front."
                >
                  <input
                    type="checkbox"
                    role="switch"
                    aria-label={t("Agents may use this Mac")}
                    checked={current().enabled}
                    disabled={busy()}
                    onChange={(event) => void request({ action: "enable", enabled: event.currentTarget.checked })}
                  />
                </Row>
                <Show when={!current().console}>
                  <p class="notice-inline computer-note">
                    {t("This Mac's screen belongs to another signed-in user, so agents can't use it from this account.")}
                  </p>
                </Show>
                <Show when={current().console && current().screen?.locked}>
                  <p class="notice-inline computer-note">{t("This Mac's screen is locked. Agents wait until you unlock it.")}</p>
                </Show>
                <Show when={current().console && !current().screen?.locked && current().screen?.asleep}>
                  <p class="notice-inline computer-note">{t("This Mac's display is asleep. Agents wait until it wakes.")}</p>
                </Show>

                <h3>{t("macOS permissions")}</h3>
                <Show when={host()}>
                  <p class="muted computer-lead">
                    {current().host?.haarness
                      ? t("macOS asks for these for Haarness.")
                      : t("macOS asks for these for {app}, the app Haarness runs in. Everything started from it shares them.", { app: host() })}
                  </p>
                </Show>
                <Row
                  title="Accessibility"
                  description="Needed to read app contents and to click, type and press keys."
                >
                  {permission(current().permissions?.accessibility, "accessibility", t("Accessibility"))}
                </Row>
                <Row
                  title="Screen Recording"
                  description="Required for screenshots. Agents can still read apps without it, just not see them."
                >
                  {permission(current().permissions?.screenRecording, "screen-recording", t("Screen Recording"))}
                </Row>

                <h3>{t("Apps")}</h3>
                <Row
                  title="Apps without a rule"
                  description="Ask first means one approval per app in each chat, given by Auto's reviewer in Auto when it is on."
                >
                  <Select
                    aria-label={t("Apps without a rule")}
                    value={current().settings.defaultAct}
                    options={defaultChoices.map((choice) => ({ value: choice.value, label: t(choice.label) }))}
                    disabled={busy()}
                    onChange={(value) => void request({ action: "configure", defaultAct: value })}
                  />
                </Row>
                <Row
                  title="Mark each click on screen"
                  description="A ring marks the spot for a moment before each click."
                >
                  <input
                    type="checkbox"
                    role="switch"
                    aria-label={t("Mark each click on screen")}
                    checked={current().settings.ring}
                    disabled={busy()}
                    onChange={(event) => void request({ action: "configure", ring: event.currentTarget.checked })}
                  />
                </Row>
                <div class="computer-apps" data-setting="App rules">
                  <div class="computer-apps-head">
                    <strong>{t("App rules")}</strong>
                    <p>
                      {t("Password managers and Haarness itself are never used, and agents never type into terminals. In Auto, apps set to ask every time still ask you.")}
                    </p>
                  </div>
                  <Show when={current().settings.apps.length}>
                    <ul class="computer-app-list">
                      <For each={current().settings.apps}>
                        {(item) => (
                          <li>
                            <span class="computer-app-name">
                              <bdi>{item.name ?? item.bundle}</bdi>
                              <Show when={item.name}>
                                <small dir="ltr">{item.bundle}</small>
                              </Show>
                            </span>
                            <Select
                              aria-label={t("Rule for {app}", { app: item.name ?? item.bundle })}
                              value={item.rule}
                              options={ruleChoices.map((choice) => ({ value: choice.value, label: t(choice.label) }))}
                              disabled={busy()}
                              onChange={(value) => void request({ action: "app", bundle: item.bundle, rule: value })}
                            />
                            <button
                              type="button"
                              class="icon-button"
                              disabled={busy()}
                              aria-label={t("Remove the rule for {app}", { app: item.name ?? item.bundle })}
                              onClick={() => void request({ action: "app", bundle: item.bundle, rule: "clear" })}
                            >
                              <Icon name="close" size="sm" />
                            </button>
                          </li>
                        )}
                      </For>
                    </ul>
                  </Show>
                  <form class="computer-app-add" onSubmit={addRule}>
                    <input
                      aria-label={t("App bundle identifier")}
                      placeholder={t("com.apple.TextEdit or com.jetbrains.*")}
                      spellcheck={false}
                      autocomplete="off"
                      dir="ltr"
                      value={bundle()}
                      aria-invalid={invalid()}
                      onInput={(event) => {
                        setBundle(event.currentTarget.value);
                        setInvalid(false);
                      }}
                      disabled={busy()}
                    />
                    <Select
                      aria-label={t("Rule for the new app")}
                      value={rule()}
                      options={ruleChoices.map((choice) => ({ value: choice.value, label: t(choice.label) }))}
                      disabled={busy()}
                      onChange={(value) => setRule(value as ComputerRule)}
                    />
                    <button disabled={busy() || !bundle().trim()}>{t("Add app")}</button>
                  </form>
                  <Show when={invalid()}>
                    <p class="computer-invalid" role="alert">
                      {t("Use a bundle identifier like com.apple.TextEdit.")}
                    </p>
                  </Show>
                </div>

                <h3>{t("Terms")}</h3>
                <Row title="Terms for computer use" description="You agree to these before agents use this Mac.">
                  <button type="button" aria-expanded={showTerms()} onClick={() => setShowTerms((value) => !value)}>
                    {showTerms() ? t("Hide terms") : t("View terms")}
                  </button>
                  <button type="button" disabled={busy()} onClick={() => void request({ action: "withdraw" })}>
                    {t("Withdraw agreement")}
                  </button>
                </Row>
                <Show when={agreedOn()}>
                  <p class="muted computer-agreed">{t("You agreed on {date}.", { date: agreedOn() })}</p>
                </Show>
                <Show when={showTerms()}>
                  <Terms status={current()} />
                </Show>
              </Show>
            </Show>
          )}
        </Show>
      </Show>
    </div>
  );
}

/** The terms as the server sends them, each sentence in the interface language. */
function Terms(props: { status: ComputerUseStatus }) {
  const text = () => props.status.terms.text;
  return (
    <Show when={text()}>
      {(terms) => (
        <div class="computer-terms" data-testid="computer-terms">
          <p>{t(terms().intro)}</p>
          <For each={terms().sections}>
            {(section) => (
              <section>
                <strong>{t(section.heading)}</strong>
                <ul>
                  <For each={section.items}>{(item) => <li>{t(item)}</li>}</For>
                </ul>
              </section>
            )}
          </For>
        </div>
      )}
    </Show>
  );
}

/**
 * Before computer use is on for the first time, or again after the terms changed (said in one quiet line beside the
 * agreement): the terms, and one way to agree to them.
 */
function Agreement(props: {
  status: ComputerUseStatus;
  busy: boolean;
  agreed: boolean;
  setAgreed: (value: boolean) => void;
  agree: () => void;
}) {
  return (
    <div class="computer-agreement" data-setting="Terms for computer use">
      <strong class="computer-agreement-title">{t(props.status.terms.text?.title ?? "Terms for computer use")}</strong>
      <Terms status={props.status} />
      <Show when={props.status.terms.changed}>
        <p class="muted computer-terms-changed" data-testid="computer-terms-changed">
          {t("You agreed to an earlier version of these terms.")}
        </p>
      </Show>
      <label class="computer-agree">
        <input
          type="checkbox"
          checked={props.agreed}
          disabled={props.busy}
          onChange={(event) => props.setAgreed(event.currentTarget.checked)}
        />
        <span>{t("I have read and agree to these terms")}</span>
      </label>
      <div class="computer-agreement-actions">
        <button class="primary" disabled={!props.agreed || props.busy} onClick={() => props.agree()}>
          {t("Agree and turn on")}
        </button>
      </div>
    </div>
  );
}

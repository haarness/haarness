import { Show, type JSX } from "solid-js";
import type { UpdateAction, UpdateState } from "../updates";
import { language, t } from "./i18n";
import { Icon } from "./icons";
import { SettingRow as Row } from "./SettingRow";
import { useApp } from "./state";

/** Asks the main process; what happens, failures included, comes back as update state. */
const act = (action: UpdateAction) => void window.haarness.update(action).catch(() => {});

/** A failure that mends itself (the service out of reach for a while), as
 * opposed to one the person may need to act on. */
const passing = (update: UpdateState) => update.error?.code === "offline" || update.error?.code === "service";

/** Why the last check, download or install failed, in the person's words. */
function problem(update: UpdateState) {
  switch (update.error?.code) {
    case undefined:
      return "";
    case "offline":
      return t("Couldn’t reach the update service. Haarness will try again.");
    case "service":
      return t("The update service gave an unexpected answer. Haarness will try again.");
    case "damaged":
      return t("The download was damaged. Haarness will try again.");
    case "space":
      return t("There isn’t enough free disk space for the update.");
    case "write":
      return t("Haarness couldn’t save the update.");
    case "signature":
      return t("The update’s signature didn’t verify, so it wasn’t installed.");
    case "install":
      return t("The last update couldn’t be installed. Haarness stayed on this version.");
    default:
      return t("The update failed.");
  }
}

/** What clicking the update row (or About's main button) does, by state. */
function primary(update: UpdateState): { label: string; action: UpdateAction } | undefined {
  const version = update.version ?? "";
  if (update.method === "move" && update.version) return { label: t("Move to Applications"), action: "move" };
  if (update.method === "installer") {
    if (update.status === "ready") return { label: t("Open installer"), action: "open-installer" };
    if (update.status === "available") return { label: t("Download installer"), action: "open-installer" };
    return undefined;
  }
  if (update.method !== "restart") return undefined;
  if (update.status === "ready") return { label: t("Restart to update"), action: "restart" };
  if (update.status === "available") return { label: t("Download {version}", { version }), action: "download" };
  return undefined;
}

/**
 * The sidebar's update row, above the account: it appears only when there is
 * something to do (a restart installs a version that is ready, or a newer one
 * waits for a click), never while a download runs in the background by itself.
 */
export function UpdateRow() {
  const app = useApp();
  const update = () => app.state.update;
  const shown = () => {
    const value = update();
    if (value.method === "off" || !value.version) return false;
    switch (value.status) {
      case "ready":
      case "installing":
        return true;
      case "available":
        // Downloading by itself, it shows once ready; it shows now when it
        // waits for the person, or failed.
        return value.method !== "restart" || !app.state.preferences.autoUpdate || !!value.error || !!value.asked;
      case "downloading":
      case "verifying":
        return !!value.asked;
      default:
        return false;
    }
  };
  const label = () => {
    const value = update();
    if (value.status === "installing") return t("Restarting…");
    if (value.status === "downloading") return t("Downloading {percent}%", { percent: value.percent ?? 0 });
    if (value.status === "verifying") return t("Preparing…");
    if (value.status === "ready" && value.method === "restart") return t("Restart to update");
    return t("Update to {version}", { version: value.version ?? "" });
  };
  const click = () => {
    const value = update();
    // Moving the app is explained in About first; everything else acts at once.
    if (value.method === "move") return void app.openSettings("about");
    const next = primary(value);
    if (next) act(next.action);
  };
  const busy = () => ["downloading", "verifying", "installing"].includes(update().status);
  const failed = () => !!update().error && !passing(update()) && !busy();
  return (
    <Show when={shown()}>
      <button
        class="nav-row update-row"
        classList={{ ready: update().status === "ready", failed: failed() }}
        disabled={busy()}
        title={problem(update()) || t("Haarness {version} is available.", { version: update().version ?? "" })}
        onClick={click}
      >
        <Icon name={failed() ? "alert" : "update"} size="md" />
        <span>{label()}</span>
        <Show when={update().status === "downloading"}>
          <span class="update-progress" aria-hidden="true" style={{ "--progress": `${update().percent ?? 0}%` }} />
        </Show>
      </button>
    </Show>
  );
}

/** When the last check reached the release service: a time today, else a date and time. */
function checkedAt(at: number) {
  const date = new Date(at);
  const today = date.toDateString() === new Date().toDateString();
  return today
    ? date.toLocaleTimeString(language(), { hour: "numeric", minute: "2-digit" })
    : date.toLocaleString(language(), { month: "short", day: "numeric", hour: "numeric", minute: "2-digit" });
}

/** Settings → About: what the updater knows, its one next step, and the automatic download switch. */
export function UpdateSettings() {
  const app = useApp();
  const update = () => app.state.update;
  const status = (): string => {
    const value = update();
    const version = value.version ?? "";
    if (value.method === "off") return t("Updates are off in development builds.");
    switch (value.status) {
      case "installing":
        return t("Restarting…");
      case "verifying":
        return t("Preparing…");
      case "downloading":
        return t("Downloading {percent}%", { percent: value.percent ?? 0 });
      case "ready":
        return value.method === "restart"
          ? t("Haarness {version} is ready to install.", { version })
          : t("Haarness {version} is available.", { version });
      case "available":
        return t("Haarness {version} is available.", { version });
    }
    if (value.checking) return t("Checking for updates…");
    if (value.method === "move") return t("Move Haarness to Applications to install updates.");
    return value.checkedAt ? t("You are up to date.") : "";
  };
  const hint = (): string => {
    const value = update();
    if (value.method === "move" && value.version) return t("Move Haarness to Applications to install updates.");
    if (value.method === "installer" && value.version)
      return t("This copy of Haarness can’t replace itself, so updates come as an installer.");
    return "";
  };
  const next = () => primary(update());
  const busy = () => ["downloading", "verifying", "installing"].includes(update().status);
  const detail = (): JSX.Element => (
    <div class="update-status" role="status" aria-live="polite">
      <Show when={status()}>
        <p>{status()}</p>
      </Show>
      <Show when={update().status === "downloading"}>
        <progress max="100" value={update().percent ?? 0} aria-label={t("Downloading {percent}%", { percent: update().percent ?? 0 })} />
      </Show>
      <Show when={hint()}>
        <p class="muted">{hint()}</p>
      </Show>
      <Show when={problem(update())}>
        {/* Out of reach for now is said quietly, as it mends itself; a
            damaged or refused update is said in the failure colour. */}
        <p
          classList={{ "update-problem": !passing(update()) }}
          title={update().error?.detail}
        >
          {problem(update())}
        </p>
      </Show>
      <Show when={update().checkedAt && update().status === "idle" && !update().checking}>
        <small class="muted">{t("Last checked {time}", { time: checkedAt(update().checkedAt!) })}</small>
      </Show>
      <Show when={update().notesUrl && update().status !== "idle"}>
        <a
          href={update().notesUrl}
          onClick={(event) => {
            event.preventDefault();
            void window.haarness.openExternal(update().notesUrl!).catch(() => {});
          }}
        >
          {t("Release notes")}
        </a>
      </Show>
    </div>
  );
  return (
    <>
      <Row title="Updates" detail={detail()}>
        <Show
          when={next()}
          fallback={
            <Show when={update().method !== "off" && !busy()}>
              <button disabled={update().checking} onClick={() => act("check")}>
                {t("Check for updates")}
              </button>
            </Show>
          }
        >
          {(value) => (
            <button class="primary" disabled={busy()} onClick={() => act(value().action)}>
              {value().label}
            </button>
          )}
        </Show>
      </Row>
      <Show when={update().method === "restart"}>
        <Row
          title="Download updates automatically"
          description="New versions download in the background and install when Haarness restarts."
        >
          <input
            type="checkbox"
            role="switch"
            aria-label={t("Download updates automatically")}
            checked={app.state.preferences.autoUpdate}
            onChange={(event) => app.run(() => app.settings({ autoUpdate: event.currentTarget.checked }))}
          />
        </Row>
      </Show>
    </>
  );
}

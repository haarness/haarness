import { createStore } from "solid-js/store";
import { cancelled } from "./error-text";
import { errorLine, t } from "./i18n";

/**
 * Remote access: whether this whole computer can be reached from Haarness
 * Cloud, so the Haarness app on a phone and any browser signed in to the
 * same account open its projects, chats, files and terminal. The title bar's
 * control turns it on and off. It belongs to the account, so there is no
 * code or link to pass on; `/remote-control` shares a single chat instead.
 * Haarness runs the connector; this module reads and changes its state.
 */
export type AccessState = "signed_out" | "off" | "connecting" | "on";
export interface Access {
  state: AccessState;
  /** The name phones and the web show for this computer. */
  computer?: string;
  /** An organization account whose members can open it too. */
  organization?: string;
  error?: string;
}
type Notify = (text: string, tone?: "done" | "blocked") => void;

const states: AccessState[] = ["signed_out", "off", "connecting", "on"];

/** The answer of the `remote-access` account request, checked. */
export function parseAccess(value: unknown): Access {
  const data = (value ?? {}) as Record<string, unknown>;
  const text = (value: unknown) =>
    typeof value === "string" && value.trim() ? value : undefined;
  return {
    state: states.includes(data.state as AccessState)
      ? (data.state as AccessState)
      : "off",
    computer: text(data.computer),
    organization: text(data.organization),
    error: text(data.error),
  };
}

/** How long until the state is read again: soon while Cloud has yet to see
 * the connector, now and then while it is on (it can be removed from a
 * phone), and not at all while it is off. */
export function refreshAfter(access?: Access) {
  if (access?.state === "connecting") return 2_000;
  if (access?.state === "on") return 30_000;
  return 0;
}

/** What pressing the control does: sign in first when there is no account
 * to be reached from. Off with an error (the connector gave up) tries again. */
export function pressAction(access?: Access): "on" | "off" | "sign-in" {
  if (access?.state === "signed_out") return "sign-in";
  return access?.state === "on" || access?.state === "connecting" ? "off" : "on";
}

const [remoteAccess, setRemoteAccess] = createStore<{
  access?: Access;
  /** A change on its way: shown before Cloud answers. */
  busy: "" | "on" | "off";
}>({ busy: "" });
export { remoteAccess };

// Every read and every change takes the next number, and a read's answer
// counts only while it is still the latest, so an answer that was overtaken
// cannot put back an older state.
let revision = 0;
let timer: ReturnType<typeof setTimeout> | undefined;
let lastRead = 0;
let watching = false;
/** Pressed while signed out: turn access on once the sign-in lands, if it
 * lands while the person is still at it (a sign-in takes at most 16 minutes). */
let afterSignIn: { notify: Notify; until: number } | undefined;

function show(access: Access) {
  setRemoteAccess("access", access);
  clearTimeout(timer);
  const after = refreshAfter(access);
  if (after) timer = setTimeout(() => void read(true), after);
}

async function read(check: boolean) {
  if (remoteAccess.busy) return;
  const asked = ++revision;
  lastRead = Date.now();
  try {
    const access = parseAccess(
      await window.haarness.account({ action: "remote-access", check }),
    );
    if (asked === revision) show(access);
  } catch {
    // What is shown stays; the next read tries again.
    if (asked === revision && remoteAccess.access) show(remoteAccess.access);
  }
}

/** Follow remote access in this window: what this computer knows at once,
 * then what Cloud sees, and again after a change in any window, a sign-in,
 * sign-out or account switch, or when the window comes back to the front. */
export function watchRemoteAccess() {
  if (watching) return;
  watching = true;
  void read(false).then(() => read(true));
  window.haarness.onEvent((event) => {
    if (event.kind !== "menu") return;
    const data = event.data as { action?: string; access?: unknown };
    if (data.action === "remote-access") {
      revision++;
      show(parseAccess(data.access));
    } else if (data.action === "account-changed") {
      const pending = afterSignIn && Date.now() < afterSignIn.until ? afterSignIn : undefined;
      afterSignIn = undefined;
      void read(true).then(() => {
        if (pending && remoteAccess.access?.state === "off") void change("on", pending.notify);
      });
    }
  });
  window.addEventListener("focus", () => {
    if (Date.now() - lastRead > 10_000) void read(true);
  });
}

async function change(to: "on" | "off", notify: Notify) {
  if (remoteAccess.busy) return;
  revision++;
  clearTimeout(timer);
  setRemoteAccess("busy", to);
  let access: Access | undefined;
  try {
    access = parseAccess(
      await window.haarness.account({ action: "remote-access", change: to }),
    );
  } catch (error) {
    setRemoteAccess("busy", "");
    if (!cancelled(error)) notify(errorLine(error), "blocked");
    void read(true);
    return;
  }
  revision++;
  setRemoteAccess("busy", "");
  show(access);
  if (access.state === "signed_out") return void signIn(notify);
  if (access.error) return void notify(access.error, "blocked");
  if (to === "off") return void notify(t("Remote access is off"));
  notify(
    t("Remote access is on. Open {computer} from Haarness on your phone or any browser.", {
      computer: access.computer ?? t("This computer"),
    }),
  );
}

function signIn(notify: Notify) {
  afterSignIn = { notify, until: Date.now() + 16 * 60_000 };
  window.dispatchEvent(new CustomEvent("haarness:account"));
}

/** The title bar control was pressed. */
export function pressRemoteAccess(notify: Notify) {
  const action = pressAction(remoteAccess.access);
  if (action === "sign-in") signIn(notify);
  else void change(action, notify);
}

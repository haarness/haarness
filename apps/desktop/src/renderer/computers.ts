import { createStore } from "solid-js/store";
import type { Computer } from "../shared";
import { sortComputers } from "../servers";
import { language, t } from "./i18n";
import { relativeAge } from "./relative-age";

type Status = "idle" | "loading" | "ready" | "signed-out" | "error";
const [computers, setComputers] = createStore<{
  status: Status;
  items: Computer[];
  error: string;
}>({ status: "idle", items: [], error: "" });
export { computers };

let pending: Promise<void> | undefined;
let generation = 0;
let listening = false;
let viewers = 0;

/** Reads your other computers from the account. Only a list of servers that
 * opens asks for them, never the app starting; the list read before stays in
 * view while it is read again. */
export function loadComputers() {
  if (!listening) {
    listening = true;
    // The list belongs to the account that read it.
    window.haarness.onEvent((event) => {
      if (event.kind === "menu" && (event.data as { action?: string })?.action === "account-changed") {
        generation++;
        pending = undefined;
        setComputers({ status: "idle", items: [], error: "" });
        if (viewers) void loadComputers();
      }
    });
  }
  if (pending) return pending;
  const asked = generation;
  if (computers.status !== "ready") setComputers({ status: "loading", error: "" });
  const request: Promise<void> = window.haarness
    .account({ action: "remote-hosts" })
    .then((value: { loggedIn?: boolean; items?: Computer[] }) => {
      if (asked !== generation) return;
      if (value?.loggedIn === false) setComputers({ status: "signed-out", items: [], error: "" });
      else setComputers({ status: "ready", items: sortComputers(value?.items ?? [], language()), error: "" });
    })
    .catch((error: unknown) => {
      if (asked !== generation) return;
      const message = error instanceof Error ? error.message : String(error);
      // A list already shown stays; the next look reads it again.
      setComputers(computers.items.length ? { status: "ready", error: "" } : { status: "error", error: message });
    })
    .finally(() => {
      if (pending === request) pending = undefined;
    });
  pending = request;
  return request;
}

/** Keeps the list read while a view shows it: now, and again after a
 * sign-in, sign-out or account switch. Returns the way to stop. */
export function watchComputers() {
  viewers++;
  void loadComputers();
  return () => {
    viewers--;
  };
}

/** When a computer that is not connected now was last, in the reader's words. */
export function lastSeen(computer: Computer, now = Date.now()) {
  const at = Date.parse(computer.lastSeenAt ?? "");
  if (!Number.isFinite(at)) return t("Not connected");
  if (now - at < 60_000) return t("Last seen just now");
  const age = relativeAge(computer.lastSeenAt!, now, language(), "");
  // Past four weeks relativeAge gives the date itself.
  return now - at < 28 * 86_400_000
    ? t("Last seen {age} ago", { age })
    : t("Last seen {date}", { date: age });
}

/** The operating system a computer reported, as people call it. */
export function platformName(platform?: string | null) {
  return platform === "macos" ? "macOS" : platform === "windows" ? "Windows" : platform === "linux" ? "Linux" : "";
}

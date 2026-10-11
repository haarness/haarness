/**
 * A page's line to the service worker: a `ui` port that delivers a snapshot on
 * every change and carries requests. Holding the port open also tells the
 * worker someone is watching, so it retries a lost connection every 2 s.
 */
import { createSignal } from "solid-js";
import { key } from "../core/i18n";
import { failure, type UiRequest, type UiResponse, type UiSnapshot } from "../core/ui";
import { applyLanguage } from "./i18n";

const [snapshot, setSnapshot] = createSignal<UiSnapshot>();
export { snapshot };

let port: chrome.runtime.Port | undefined;
let next = 0;
const pending = new Map<number, (response: UiResponse) => void>();

export function connect() {
  port = chrome.runtime.connect({ name: "ui" });
  port.onMessage.addListener((message: { type: string; snapshot?: UiSnapshot; id?: number; response?: UiResponse }) => {
    if (message.type === "snapshot" && message.snapshot) {
      setSnapshot(message.snapshot);
      void applyLanguage(message.snapshot.settings.language);
    }
    if (message.type === "response" && message.id !== undefined) {
      pending.get(message.id)?.(message.response ?? failure(key("Haarness did not answer.")));
      pending.delete(message.id);
    }
  });
  port.onDisconnect.addListener(() => {
    // The worker restarted (an update, or Chrome stopped it); find the new one.
    port = undefined;
    for (const resolve of pending.values()) resolve(failure(key("Haarness for Chrome restarted. Try again.")));
    pending.clear();
    setTimeout(connect, 300);
  });
}

export function request(message: UiRequest): Promise<UiResponse> {
  return new Promise((resolve) => {
    if (!port) return resolve(failure(key("Haarness for Chrome is restarting. Try again.")));
    const id = ++next;
    pending.set(id, resolve);
    port.postMessage({ id, request: message });
  });
}

/** Opens the welcome page (the extension's options page) in a tab. */
export function openWelcome() {
  void chrome.runtime.openOptionsPage();
}

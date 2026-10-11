/**
 * The service worker: the only part of the extension that talks to Haarness.
 * Every listener is registered synchronously below, before anything awaits.
 */
import { createHost } from "./core/host";
import { loadDictionary, lookup, parseLanguage, type Dictionary } from "./core/i18n";
import type { UiRequest, UiResponse } from "./core/ui";
import { createChromePlatform } from "./platform/chrome";

const read = (path: string) => fetch(chrome.runtime.getURL(path)).then((response) => response.json());
let english: Dictionary = {};
let dictionary: Dictionary = {};
void loadDictionary("en", read).then((value) => (english = value));

const { platform, listen } = createChromePlatform();
const host = createHost({
  platform,
  translate: (key) => lookup(dictionary, english, key),
  language: (code) => void loadDictionary(parseLanguage(code), read).then((value) => (dictionary = value)),
});
listen(host.events);

// The popup and welcome page: same-extension pages only.
chrome.runtime.onConnect.addListener((port) => {
  if (port.sender?.id !== chrome.runtime.id || port.name !== "ui") return port.disconnect();
  const stop = host.watch((snapshot) => port.postMessage({ type: "snapshot", snapshot }));
  port.onMessage.addListener((message: { id: number; request: UiRequest }) => {
    const reply = (response: UiResponse) => {
      try {
        port.postMessage({ type: "response", id: message.id, response });
      } catch {
        // The page closed before the answer.
      }
    };
    void host.request(message.request).then(reply, (error: unknown) =>
      reply({ ok: false, error: error instanceof Error ? error.message : String(error) }),
    );
  });
  port.onDisconnect.addListener(stop);
});

chrome.runtime.onInstalled.addListener((details) => {
  if (details.reason === "install") void chrome.tabs.create({ url: chrome.runtime.getURL("welcome.html") });
});

void chrome.commands.getAll().then((commands) => {
  host.setShortcut(commands.find((command) => command.name === "stop-all")?.shortcut || null);
});

// Read by the end-to-end tests through Playwright's service worker handle.
(globalThis as { haarness?: typeof host }).haarness = host;

void host.start();

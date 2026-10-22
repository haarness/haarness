// Where the keyboard goes when a browser page that has it is put away, and when the person loads a page.
import type { BrowserWindow, WebContents } from "electron";

type Window = Pick<BrowserWindow, "isDestroyed" | "isFocused"> & {
  webContents: Pick<WebContents, "focus" | "executeJavaScript">;
};
type PageContents = Pick<WebContents, "isDestroyed" | "isFocused">;
type ShownPage = Pick<WebContents, "isDestroyed" | "focus">;

/** The message box, as the renderer marks it for the main process and tests alike. */
const COMPOSER_FOCUSED = `document.activeElement?.matches?.("[data-composer-input]") === true`;

/**
 * A page put away (hidden or closed) while it has the keyboard would leave
 * the window with no one to type to, so the keyboard goes back to the
 * message box, which keeps its caret. Only while the window is the one the
 * person is typing in, so the app never comes in front of another
 * application, and only when the message box is where the window's own
 * keyboard last was: a button or menu left focused there must not receive
 * keys the person meant for the page.
 */
export async function returnKeyboard(win: Window, page: PageContents): Promise<boolean> {
  if (win.isDestroyed() || page.isDestroyed() || !win.isFocused() || !page.isFocused())
    return false;
  const composer = await win.webContents
    .executeJavaScript(COMPOSER_FOCUSED)
    .catch(() => false);
  if (composer !== true || win.isDestroyed() || !win.isFocused()) return false;
  win.webContents.focus();
  return true;
}

/**
 * Whether the window's own keyboard is in a field the person types in (the
 * message box, find, a terminal, the file editor), other than the Browser's
 * address bar, whose Enter is what loads a page.
 */
const TYPING_ELSEWHERE = `(() => {
  const element = document.activeElement;
  if (!element || element.closest?.("[data-browser-address]")) return false;
  if (element.isContentEditable || element instanceof HTMLTextAreaElement || element instanceof HTMLSelectElement) return true;
  return element instanceof HTMLInputElement &&
    !["button", "checkbox", "color", "file", "image", "radio", "range", "reset", "submit"].includes(element.type);
})()`;

/**
 * The keyboard a load the person made asks for. A page the person loads
 * themself (an address they typed, Back, Forward, Reload, a link they opened)
 * takes the keyboard, as a browser's page does, so its own field gets what
 * they type next; the agent's loads never ask.
 *
 * `want` is called when the request arrives, so its time runs from then even
 * when the command waits behind the agent's: a load waits at most `wait`
 * milliseconds to be shown. A new tab's request names its tab once its
 * command has started (`bind`). Any key or pointer press in the window after
 * the request (`input`) means the person has moved on, and drops it. `offer`
 * is called whenever a tab is drawn and gives that tab's page the keyboard
 * when it is the one wanted, only while the window is the one in use (the
 * app never comes in front of another application), and never while the
 * person's keyboard is in a field of the window's own.
 */
export function createKeyboardHandoff(win: Window, wait = 3000, now = () => Date.now()) {
  let wanted: { tab?: string; until: number } | undefined;
  return {
    want(tab?: string) {
      wanted = { tab, until: now() + wait };
    },
    bind(tab: string) {
      if (wanted && wanted.tab === undefined) wanted.tab = tab;
    },
    input() {
      wanted = undefined;
    },
    async offer(shown: string | undefined, page: ShownPage | undefined): Promise<boolean> {
      const request = wanted;
      if (!request) return false;
      const live = () =>
        wanted === request && now() <= request.until && !win.isDestroyed() && win.isFocused();
      if (!live()) {
        if (wanted === request) wanted = undefined;
        return false;
      }
      if (!page || request.tab === undefined || shown !== request.tab || page.isDestroyed()) return false;
      const typing = await win.webContents.executeJavaScript(TYPING_ELSEWHERE).catch(() => true);
      if (!live() || page.isDestroyed()) return false;
      wanted = undefined;
      if (typing === true) return false;
      page.focus();
      return true;
    },
  };
}

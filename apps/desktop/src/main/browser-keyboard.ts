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
 * A page the person loads themself (an address they typed, Back, Forward,
 * Reload, a link they opened) takes the keyboard, as a browser's page does,
 * so its own field gets what they type next. Only once the page is shown and
 * while the window is the one the person is using: the app never comes in
 * front of another application. The agent's loads never come here.
 */
export function giveKeyboard(win: Window, page: ShownPage, shown: boolean): boolean {
  if (!shown || win.isDestroyed() || page.isDestroyed() || !win.isFocused()) return false;
  page.focus();
  return true;
}

/**
 * The keyboard a load the person made asks for. `want` records its tab when
 * the load starts; `offer` is called whenever a tab is drawn and gives that
 * tab's page the keyboard once it is the one wanted (a new tab is drawn when
 * the window lays it out). A load gives up after `wait` milliseconds, or as
 * soon as the window is not the one in use, so a page never takes the
 * keyboard later, when the person has moved on.
 */
export function createKeyboardHandoff(win: Window, wait = 3000, now = () => Date.now()) {
  let wanted: { tab: string; until: number } | undefined;
  return {
    want(tab: string) {
      wanted = { tab, until: now() + wait };
    },
    offer(shown: string | undefined, page: ShownPage | undefined) {
      if (!wanted) return false;
      if (now() > wanted.until || win.isDestroyed() || !win.isFocused()) {
        wanted = undefined;
        return false;
      }
      if (!page || shown !== wanted.tab || !giveKeyboard(win, page, true)) return false;
      wanted = undefined;
      return true;
    },
  };
}

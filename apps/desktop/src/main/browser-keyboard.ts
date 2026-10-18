// Where the keyboard goes when a browser page that has it is put away.
import type { BrowserWindow, WebContents } from "electron";

type Window = Pick<BrowserWindow, "isDestroyed" | "isFocused"> & {
  webContents: Pick<WebContents, "focus" | "executeJavaScript">;
};
type PageContents = Pick<WebContents, "isDestroyed" | "isFocused">;

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

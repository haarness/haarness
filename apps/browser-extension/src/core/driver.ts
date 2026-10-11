/**
 * The one door to page operations. Everything that reads or acts on a page's
 * content (navigate, snapshot, click, screenshot, …) goes through `createPage`;
 * tab management (`tabs.*`) never does.
 *
 * At integration this becomes `export { createPage } from
 * "@haarness/browser-driver/page"`. Until then a page command fails with a
 * plain reason instead of half-working.
 */
import type { CreatePage } from "./driver-contract";

export const DRIVER_NOT_WIRED =
  "This build of Haarness for Chrome has no page driver yet, so only browser_tabs_* tools work in Chrome. Use the Haarness browser in Desktop for page actions.";

export const createPage: CreatePage = () => {
  throw new Error(DRIVER_NOT_WIRED);
};

/** Whether `createPage` can run. Lets the host skip attaching a debugger it cannot use. */
export const driverWired = false as boolean;

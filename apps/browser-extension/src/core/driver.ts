/**
 * The one door to page operations. Everything that reads or acts on a page's
 * content (navigate, snapshot, click, screenshot, …) goes through the shared
 * driver's `createPage`; tab management (`tabs.*`) never does.
 */
export { createPage, lastRef } from "@haarness/browser-driver/page";

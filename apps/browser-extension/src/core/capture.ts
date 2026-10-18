/**
 * Chrome paints only the visible tab of each window, so a screenshot of a
 * background tab needs that tab shown first. Haarness never takes the screen
 * from the user: it shows the tab only in a window the user is not using, and
 * puts the window's previous tab back right after the capture.
 */

export type CapturePlan =
  /** The tab is already painted. */
  | "ready"
  /** Show the tab briefly in its (unfocused) window, then restore. */
  | "activate"
  /** The user is in that window; fail with `background-tab`. */
  | "refuse";

export function capturePlan(tab: { active: boolean; windowId: number }, focusedWindow: number | undefined): CapturePlan {
  if (tab.active) return "ready";
  return focusedWindow === tab.windowId ? "refuse" : "activate";
}

/**
 * What `browser_tabs_focus` does to a tab. The tab always becomes the chat's
 * focused tab; it comes forward only where that takes nothing from the user:
 * in a window the user is not using, or over another of the chat's own tabs.
 */
export type FocusPlan =
  /** The tab is already the one its window shows. */
  | "ready"
  /** Show the tab in its window. */
  | "activate"
  /** The user is using that window: leave the tab in the background. */
  | "keep";

export function focusPlan(
  tab: { active: boolean; windowId: number },
  focusedWindow: number | undefined,
  overOwnTab: boolean,
): FocusPlan {
  if (tab.active) return "ready";
  return focusedWindow !== tab.windowId || overOwnTab ? "activate" : "keep";
}

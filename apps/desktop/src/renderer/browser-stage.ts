/**
 * When the agent's pages may bring the Browser on screen. The person's own
 * pages always show it; an agent's open a closed side panel, but never take
 * it from another panel (Review, Files, Context…) and never bring back a
 * Browser the person put away: those leave a dot on the Browser instead.
 */

/** Where the Browser stands in the window, for one chat. */
export interface BrowserStage {
  /** The chat the window shows (connection and chat id). */
  chat: string;
  /** The Browser is on screen. */
  shown: boolean;
  /** The side panel is hidden. */
  hidden: boolean;
  /** The side panel shows only its panel launcher (the person is picking a panel). */
  launcher: boolean;
  /** The Browser has a tab in the side panel. */
  tab: boolean;
}

/**
 * Whether the person put the Browser away, from the stage before (`prev`)
 * and now: never while it is on screen; put away once it leaves the screen
 * in the same chat because the person hid the side panel or closed the
 * Browser's tab. Anything else, such as picking another panel or moving to
 * another chat, leaves it as it was (`undefined`).
 */
export function putAway(prev: BrowserStage | undefined, next: BrowserStage): boolean | undefined {
  if (next.shown) return false;
  if (prev?.shown && prev.chat === next.chat && (next.hidden || !next.tab)) return true;
  return undefined;
}

/**
 * What a page the agent opened, focused or took elsewhere does: nothing when
 * the Browser is on screen already; it opens a hidden side panel, or one
 * showing only its launcher, unless the person put the Browser away;
 * anything else leaves a dot on the Browser.
 */
export function agentReveal(
  stage: Pick<BrowserStage, "shown" | "hidden" | "launcher">,
  away: boolean,
): "show" | "notify" | "none" {
  if (stage.shown) return "none";
  if ((stage.hidden || stage.launcher) && !away) return "show";
  return "notify";
}

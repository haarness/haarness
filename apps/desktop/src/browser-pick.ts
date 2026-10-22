/** An element the person picked in the Haarness browser, as its page describes it. */
export interface PickedElement {
  /** The page's address when it was picked. */
  url: string;
  /** "button#save.primary": the tag, id and first classes. */
  tag: string;
  /** Its accessible name, when it has one. */
  name: string;
  /** Its visible text, unless that is its name. */
  text: string;
  /** Its outer HTML, trimmed to about 2 KB. */
  html: string;
  /** A CSS path to it; " >>> " steps into a shadow root. Empty when too long. */
  selector: string;
  width: number;
  height: number;
}

/** What starts picking: the hint the page shows and the theme's accent. */
export interface PickRequest {
  hint: string;
  accent: string;
}

/**
 * Why picking ended: a click picked an element (`picked`), the person pressed
 * Escape (`cancelled`), the panel asked (`stopped`), or the page went away
 * (`navigated`, `hidden`, `closed`).
 */
export type PickEnd = "picked" | "cancelled" | "stopped" | "navigated" | "hidden" | "closed";

/** What the Browser panel hears while the person picks. */
export type PickEvent =
  | { type: "picked"; tab: string; element: PickedElement; more: boolean }
  | { type: "ended"; reason: PickEnd };

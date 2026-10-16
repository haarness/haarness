/**
 * Ctrl+Tab and Ctrl+Shift+Tab step through the tabs of the panel that has
 * the keyboard: the chat tabs while it is in the conversation, the terminal
 * tabs in the terminal, open files in the Files panel, pages in the browser
 * panel, and the side panel's own tabs elsewhere in the side panel. Each
 * panel registers once and marks its root with `data-cycle-panel="<id>"`;
 * the innermost marked panel with a tab wins, so a Files panel with no open
 * file steps the side panel instead.
 */
export interface CyclePanel {
  id: string;
  /** Tab ids, in the order the tab bar draws them. */
  tabs: () => readonly string[];
  /** The tab shown now. */
  active: () => string | undefined;
  /** What a click on that tab does. */
  select: (tab: string) => void;
  /**
   * Puts the keyboard back after a step that started at `from`, in place
   * of the default: leave it where it was if that is still there, else put
   * it on the new tab's button.
   */
  keep?: (tab: string, from: Element | null) => void;
}

/**
 * The tab `offset` steps from `active` in `list`, wrapping past either end.
 * From a tab the list does not hold, a step forward lands on the first and
 * a step back on the last.
 */
export function step(list: readonly string[], active: string | undefined, offset: number) {
  if (!list.length) return;
  const at = active === undefined ? -1 : list.indexOf(active);
  if (at < 0) return offset > 0 ? list[0] : list[list.length - 1];
  return list[(((at + offset) % list.length) + list.length) % list.length];
}

/** The tab button a step leaves the keyboard on: tab bars mark each tab's
 * button with `data-cycle-tab="<id>"`. */
export function tabButton(root: ParentNode | null | undefined, tab: string) {
  return root?.querySelector<HTMLElement>(`[data-cycle-tab="${CSS.escape(tab)}"]`) ?? null;
}

type Marked = {
  closest(selector: string): Marked | null;
  parentElement: Marked | null;
  getAttribute(name: string): string | null;
};

export function createCycle(
  schedule: (run: () => void) => void = (run) => void requestAnimationFrame(run),
) {
  const panels = new Map<string, CyclePanel>();
  /** The panel a step goes to from `element`, the one with the keyboard,
   * and that panel's marked root. */
  function route(element: Marked | null) {
    let root = element?.closest("[data-cycle-panel]") ?? null;
    while (root) {
      const panel = panels.get(root.getAttribute("data-cycle-panel") ?? "");
      if (panel?.tabs().length) return { panel, root };
      root = root.parentElement?.closest("[data-cycle-panel]") ?? null;
    }
  }
  return {
    /** Adds a panel; the function it returns takes it out again. */
    register(panel: CyclePanel) {
      panels.set(panel.id, panel);
      return () => {
        if (panels.get(panel.id) === panel) panels.delete(panel.id);
      };
    },
    route,
    /**
     * Steps the panel `from` (the element with the keyboard) is in, by
     * `offset`. Returns whether a panel took the key: with a single tab it
     * does, and selects nothing. With the keyboard in no panel, the key goes
     * on to what has focus.
     */
    step(from: Element | null, offset: 1 | -1) {
      const target = route(from as Marked | null);
      if (!target) return false;
      const { panel } = target;
      const root = target.root as unknown as Element;
      const tab = step(panel.tabs(), panel.active(), offset);
      if (tab === undefined) return true;
      if (tab !== panel.active()) panel.select(tab);
      // The keyboard stays in the panel: where it was (a terminal, the
      // message box, an editor), or, when that is gone or was a tab, on the
      // new tab. The new tab may draw a frame later.
      schedule(() => {
        if (panel.keep) return panel.keep(tab, from);
        const stays =
          !!from?.isConnected &&
          root.contains(from) &&
          !from.closest("[data-cycle-tab]") &&
          document.activeElement === from;
        if (!stays) tabButton(root, tab)?.focus({ preventScroll: true });
      });
      return true;
    },
  };
}

const shared = createCycle();
export const registerCyclePanel = shared.register;
export const stepFocusedPanel = shared.step;

// The sidebar's chat rows are one tab stop, as a list is (a roving
// tabindex): Tab reaches the row last focused there, or the open chat's row
// once another chat is opened, or else the first row; ↑ ↓ Home and End walk
// the rest (App.tsx). A row's archive button is for the pointer and for
// Delete, so it is never a stop of its own. Rows come and go with chats and
// projects, so the stop is placed again whenever the list changes.

/** The chat rows the sidebar draws, top to bottom. */
export const chatRowSelector = ".session-row > button[aria-current]";

export function rovingRows(list: HTMLElement) {
  let last: WeakRef<HTMLElement> | undefined;
  let open: HTMLElement | undefined;
  let frame = 0;
  const place = () => {
    frame = 0;
    const rows = [...list.querySelectorAll<HTMLElement>(chatRowSelector)];
    const current = rows.find((row) => row.getAttribute("aria-current") === "true");
    // Opening another chat (from anywhere) makes its row the stop again.
    if (current !== open) {
      open = current;
      last = undefined;
    }
    const focused = last?.deref();
    const stop = (focused && rows.includes(focused) ? focused : undefined) ?? current ?? rows[0];
    for (const row of rows) row.tabIndex = row === stop ? 0 : -1;
    for (const button of list.querySelectorAll<HTMLElement>(".session-row > .icon-button"))
      button.tabIndex = -1;
  };
  const schedule = () => {
    if (!frame) frame = requestAnimationFrame(place);
  };
  const focus = (event: FocusEvent) => {
    const row = event.target instanceof HTMLElement && event.target.matches(chatRowSelector)
      ? event.target
      : undefined;
    if (!row) return;
    last = new WeakRef(row);
    place();
  };
  const observer = new MutationObserver(schedule);
  observer.observe(list, {
    subtree: true,
    childList: true,
    attributes: true,
    attributeFilter: ["aria-current"],
  });
  list.addEventListener("focusin", focus);
  place();
  return () => {
    observer.disconnect();
    list.removeEventListener("focusin", focus);
    cancelAnimationFrame(frame);
  };
}

/** The row Delete leaves the keyboard on once `row` is archived: the one
 * below it, or above it at the end of the list. */
export function rowAfter(rows: readonly HTMLElement[], row: HTMLElement) {
  const at = rows.indexOf(row);
  return at < 0 ? undefined : (rows[at + 1] ?? rows[at - 1]);
}

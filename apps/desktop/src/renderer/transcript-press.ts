// A press held in the transcript is the reader at work there, though it may
// send no wheel, touch or key at all: held still it sends nothing, and a
// selection dragged past the transcript's top edge scrolls it by itself.
// While such a press is down, following leaves the view where it is, and an
// upward scroll it makes counts as the reader's own (Session.tsx). This module
// says whether one is down, and when the last one ends, by every way a press
// can end, the ones whose release never arrives included, so a lost release
// never leaves following held.

/** The parts of a pointer event a press is read from. */
export type PressPointer = Pick<PointerEvent, "pointerId" | "pointerType" | "isPrimary" | "button" | "buttons">;

/** `ended` runs when the last press held in the transcript ends. */
export function createTranscriptPress(ended: () => void) {
  /** Each pointer whose press is down, by id, with its kind. */
  const down = new Map<number, string>();
  /** Runs `change`, then `ended` if it let go of the last press. */
  const update = (change: () => void) => {
    const held = down.size > 0;
    change();
    if (held && !down.size) ended();
  };
  return {
    held: () => down.size > 0,
    /** A pointer went down, `inside` the transcript or elsewhere. */
    press(event: PressPointer, inside: boolean) {
      update(() => {
        // Only one primary pointer of a kind is down at a time: when a new
        // one goes down, any other of its kind held here lost its release,
        // and a new touch never reuses an old touch's id.
        if (event.isPrimary) for (const [id, kind] of down) if (kind === event.pointerType) down.delete(id);
        // The main or middle button only: the right one opens a menu, and
        // back, forward or a pen's eraser press nothing in the text.
        if (inside && (event.button === 0 || event.button === 1)) down.set(event.pointerId, event.pointerType);
        else down.delete(event.pointerId);
      });
    },
    /** A pointer moved: with no button down, its release was lost (after a drag and drop, say). */
    move(event: PressPointer) {
      if (!event.buttons) update(() => down.delete(event.pointerId));
    },
    /** A pointer lifted, or the browser took it over (pointercancel). */
    release(event: PressPointer) {
      update(() => down.delete(event.pointerId));
    },
    /** Every press ends: a menu opened and takes the release, the window lost focus, the page hid. */
    end() {
      update(() => down.clear());
    },
    /** Every press is forgotten, without `ended`: another chat is shown, or the transcript went away. */
    clear() {
      down.clear();
    },
  };
}

export type TranscriptPress = ReturnType<typeof createTranscriptPress>;

/**
 * Feeds `press` from the window's own events, read as they come in (capture),
 * so a release past the transcript, or one a control stops, is still seen.
 * `inside` says whether a pointer went down in the transcript. Returns the
 * function that stops listening.
 */
export function listenForPress(
  press: TranscriptPress,
  inside: (event: Event) => boolean,
  view: EventTarget = window,
  page: EventTarget & { readonly hidden: boolean } = document,
) {
  const listeners: [EventTarget, string, (event: Event) => void, boolean][] = [
    [view, "pointerdown", (event) => press.press(event as PointerEvent, inside(event)), true],
    [view, "pointermove", (event) => press.move(event as PointerEvent), true],
    [view, "pointerup", (event) => press.release(event as PointerEvent), true],
    [view, "pointercancel", (event) => press.release(event as PointerEvent), true],
    // A Control-click on a Mac counts as a main-button press; its context menu swallows the release.
    [view, "contextmenu", () => press.end(), true],
    // Only when the window itself loses focus.
    [view, "blur", (event) => event.target === view && press.end(), false],
    [page, "visibilitychange", () => page.hidden && press.end(), false],
  ];
  for (const [target, type, listener, capture] of listeners) target.addEventListener(type, listener, { capture });
  return () => {
    for (const [target, type, listener, capture] of listeners) target.removeEventListener(type, listener, { capture });
  };
}

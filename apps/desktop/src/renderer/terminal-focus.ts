/** A claim for a terminal the person is starting, which has no id yet. */
const STARTING = Symbol("starting");

/**
 * Whether the next terminal view to connect takes the keyboard.
 *
 * A terminal view connects a moment after it is drawn, so focusing on every
 * first connect took the keyboard from wherever the person had put it
 * since: in This computer, clicking another chat with the terminal open put
 * the keyboard in that chat's message box, then its own shell connected and
 * took it, and the next keys went to the shell. So only the person bringing
 * a terminal up claims the keyboard for it (opening the panel, picking or
 * starting a terminal, Retry), and the claim lasts until a view connects and
 * takes it, the person presses the pointer anywhere (which puts the keyboard
 * where they pressed) or types or tabs outside the terminal (`typedElsewhere`),
 * or the panel shows another folder or project. A terminal the person starts
 * claims it for itself alone: the view the panel opened with may connect
 * while it starts, and is then replaced by the new one.
 */
export function keyboardClaim() {
  /** Which view takes the keyboard as it connects: any (`true`), none, the
   * one of the terminal with that id, or the one of the terminal starting. */
  let claimed: boolean | string | typeof STARTING = false;
  return {
    /** The person brought a terminal up: the next view to connect takes the keyboard. */
    claim() {
      claimed = true;
    },
    /** The person started a terminal: only its view takes the keyboard, once
     * `started` names it. */
    starting() {
      claimed = STARTING;
    },
    /** The terminal being started is `terminal`, or did not start
     * (`undefined`). A claim a press or keys ended meanwhile stays ended. */
    started(terminal?: string) {
      if (claimed === STARTING) claimed = terminal ?? false;
    },
    /** A pointer press, or keys used elsewhere: the keyboard stays where the person put it. */
    release() {
      claimed = false;
    },
    /**
     * The panel is about to show another terminal than the one it showed.
     * When it lists the terminals of another chat's folder or another
     * project (`elsewhere`), the person opened that chat, and the keyboard
     * stays in its message box. Otherwise the one shown is gone (closed, or
     * its server replaced) or the same place was listed again, and the next
     * terminal has the keyboard if the panel had it (`kept`); a claim not
     * taken yet stands.
     */
    shown(kept: boolean, elsewhere = false) {
      if (elsewhere) claimed = false;
      else if (kept && claimed === false) claimed = true;
    },
    /** Whether the view of `terminal` that just connected takes the
     * keyboard. A claim for any view ends either way; one for another
     * terminal stands. */
    take(terminal?: string) {
      if (claimed !== true && (terminal === undefined || claimed !== terminal)) return false;
      claimed = false;
      return true;
    },
  };
}

/**
 * Whether a key pressed outside the terminal panel shows the person using
 * the keyboard where it is: typing (a character, Enter, Backspace, Delete)
 * or moving it on with Tab. A shortcut, with Command, Control or Option,
 * does not: it may be the one that brings the terminal up. Text an input
 * method composes comes as `Process` keys, and counts by the composing and
 * the input it makes (`watchKeyboard`).
 */
export function typedElsewhere(event: { key: string; ctrlKey: boolean; metaKey: boolean; altKey: boolean }) {
  if (event.ctrlKey || event.metaKey || event.altKey) return false;
  return event.key.length === 1 || ["Tab", "Enter", "Backspace", "Delete"].includes(event.key);
}

/** Whether an event's target is inside the terminal panel. */
const inTerminal = (target: EventTarget | null) =>
  !!target && typeof (target as Element).closest === "function" && !!(target as Element).closest(".terminal-panel");

/**
 * Ends `claim` as the person puts the keyboard somewhere: a pointer press
 * anywhere, and outside the terminal panel a key `typedElsewhere` counts,
 * an input method starting to compose, or any text going into a field.
 * Listens on `root` (the window) before anything else hears the event.
 */
export function watchKeyboard(root: EventTarget, claim: { release(): void }) {
  root.addEventListener("pointerdown", () => claim.release(), true);
  root.addEventListener(
    "keydown",
    (event) => {
      if (!inTerminal(event.target) && typedElsewhere(event as KeyboardEvent)) claim.release();
    },
    true,
  );
  for (const type of ["compositionstart", "beforeinput"])
    root.addEventListener(type, (event) => inTerminal(event.target) || claim.release(), true);
}

/** The window's one claim. A press or key lands before what it does, so one
 * that brings a terminal up claims after it has ended the earlier claim. */
export const terminalKeyboard = keyboardClaim();
if (typeof window !== "undefined") watchKeyboard(window, terminalKeyboard);

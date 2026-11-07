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
 * or the panel shows another folder or project.
 */
export function keyboardClaim() {
  let claimed = false;
  return {
    /** The person brought a terminal up: the next view to connect takes the keyboard. */
    claim() {
      claimed = true;
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
      else if (kept) claimed = true;
    },
    /** Whether a view that just connected takes the keyboard; the claim ends either way. */
    take() {
      const taken = claimed;
      claimed = false;
      return taken;
    },
  };
}

/**
 * Whether a key pressed outside the terminal panel shows the person using
 * the keyboard where it is: typing (a character, Enter, Backspace, Delete)
 * or moving it on with Tab. A shortcut, with Command, Control or Option,
 * does not: it may be the one that brings the terminal up.
 */
export function typedElsewhere(event: { key: string; ctrlKey: boolean; metaKey: boolean; altKey: boolean }) {
  if (event.ctrlKey || event.metaKey || event.altKey) return false;
  return event.key.length === 1 || ["Tab", "Enter", "Backspace", "Delete"].includes(event.key);
}

/** The window's one claim. A press or key lands before what it does, so one
 * that brings a terminal up claims after it has ended the earlier claim. */
export const terminalKeyboard = keyboardClaim();
if (typeof window !== "undefined") {
  window.addEventListener("pointerdown", terminalKeyboard.release, true);
  window.addEventListener(
    "keydown",
    (event) => {
      const inTerminal = event.target instanceof Element && !!event.target.closest(".terminal-panel");
      if (!inTerminal && typedElsewhere(event)) terminalKeyboard.release();
    },
    true,
  );
}

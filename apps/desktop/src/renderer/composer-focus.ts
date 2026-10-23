// Where the keyboard goes once a chat has been opened with the pointer or
// from Search: into its message box, so typing lands in the chat on screen.

/**
 * Puts the keyboard in the message box once it shows and takes input (a
 * chat still loading hides it), unless the reader has put the keyboard
 * somewhere other than `from` meanwhile. Gives up after about two seconds.
 */
export function focusComposerWhenShown(from: Element | null) {
  let frames = 120;
  const land = () => {
    const focused = document.activeElement;
    if (focused && focused !== document.body && focused !== from) return;
    const box = document.querySelector<HTMLElement>("[data-composer-input]");
    if (box?.checkVisibility({ visibilityProperty: true }) && !box.matches(":disabled"))
      box.focus();
    else if (frames-- > 0) requestAnimationFrame(land);
  };
  requestAnimationFrame(land);
}

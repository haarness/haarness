import type { ITerminalOptions } from "@xterm/xterm";

/** Lines a terminal view keeps above its screen. */
export const SCROLLBACK = 5000;

/**
 * The least contrast terminal text keeps against what it is drawn on
 * (xterm's `minimumContrastRatio`; dim text needs half). Text in a colour
 * under it, such as black on the dark page or white on the light one, is
 * made lighter or darker until it has that much. 3:1 lifts text that would
 * vanish and leaves a program's own muted colours (comments in Solarized or
 * One Dark) apart from its body text; backgrounds, and box and block
 * characters, keep their colours.
 */
export const MINIMUM_CONTRAST = 3;

/** xterm's options for a terminal view, besides its colours. */
export function terminalOptions(fontSize: number): ITerminalOptions {
  return {
    fontFamily: "SFMono-Regular, Menlo, Consolas, monospace",
    fontSize,
    cursorBlink: true,
    scrollback: SCROLLBACK,
    minimumContrastRatio: MINIMUM_CONTRAST,
    allowProposedApi: false,
  };
}

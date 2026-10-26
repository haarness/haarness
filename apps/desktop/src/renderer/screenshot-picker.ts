/** Picking screenshots in the Screenshots dialog: what each click does. */

/** Images one message can take, as the composer's own attachment limit. */
export const PICK_LIMIT = 20;

/**
 * A click on a screenshot: picked ones are numbered in the order they were
 * clicked and go into the message in that order; clicking one again takes it
 * out and closes the gap. Past the limit, a click changes nothing.
 */
export function toggleSelection(selected: number[], index: number, limit = PICK_LIMIT): number[] {
  if (selected.includes(index)) return selected.filter((value) => value !== index);
  return selected.length >= limit ? selected : [...selected, index];
}

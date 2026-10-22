import { createSignal } from "solid-js";

/**
 * What a reader opened in the transcript (a long message, a paste, a
 * command's prompt, a thought), by key. Kept for the session, so a row the
 * transcript unmounts while scrolling comes back as it was left.
 */
const [opened, setOpened] = createSignal<ReadonlySet<string>>(new Set());
export const isOpen = (key: string) => opened().has(key);
export function setOpen(key: string, open: boolean) {
  if (isOpen(key) === open) return;
  setOpened((keys) => {
    const next = new Set(keys);
    if (open) next.add(key);
    else next.delete(key);
    return next;
  });
}

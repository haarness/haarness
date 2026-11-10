import type { ContextMenuParams, MenuItemConstructorOptions } from "electron";

/** What a right-click in a window says about the place it was made. */
export type ContextMenuPlace = Pick<
  ContextMenuParams,
  "isEditable" | "editFlags" | "selectionText" | "linkURL" | "pageURL" | "misspelledWord" | "dictionarySuggestions"
>;

export interface ContextMenuActions {
  /** The interface language's text for an English catalog key. */
  t(key: string): string;
  /** Puts a spelling suggestion in place of the misspelled word. */
  replace(word: string): void;
  /** Stops marking a word as misspelled. */
  learn(word: string): void;
  copy(text: string): void;
}

/** A misspelled word offers at most this many spellings. */
const MAX_SUGGESTIONS = 5;

/**
 * The window's right-click menu, built from what Electron says about the place
 * clicked: spellings and Add to dictionary on a misspelled word, the edit
 * commands a text field allows right now, Copy on selected text and Copy link
 * on a web link. Anywhere else it is empty, and no menu opens. The edit
 * commands keep Electron's roles, so they act as the Edit menu's do, and they
 * are named in the interface language as that menu names them.
 */
export function contextMenuItems(
  place: ContextMenuPlace,
  actions: ContextMenuActions,
): MenuItemConstructorOptions[] {
  const { t } = actions;
  const groups: MenuItemConstructorOptions[][] = [];
  const word = place.misspelledWord.trim();
  if (place.isEditable && word)
    groups.push([
      ...place.dictionarySuggestions.slice(0, MAX_SUGGESTIONS).map((suggestion) => ({
        label: suggestion,
        click: () => actions.replace(suggestion),
      })),
      { label: t("Add to dictionary"), click: () => actions.learn(word) },
    ]);
  const link = webLink(place.linkURL, place.pageURL);
  if (link) groups.push([{ label: t("Copy link"), click: () => actions.copy(link) }]);
  const flags = place.editFlags;
  if (place.isEditable) {
    const edits: MenuItemConstructorOptions[] = [];
    if (flags.canCut) edits.push({ label: t("Cut"), role: "cut" });
    if (flags.canCopy) edits.push({ label: t("Copy"), role: "copy" });
    if (flags.canPaste) edits.push({ label: t("Paste"), role: "paste" });
    if (flags.canSelectAll) edits.push({ label: t("Select all"), role: "selectAll" });
    if (edits.length) groups.push(edits);
  } else if (flags.canCopy && place.selectionText.trim())
    groups.push([{ label: t("Copy"), role: "copy" }]);
  return groups.flatMap((group, index) =>
    index ? [{ type: "separator" as const }, ...group] : group,
  );
}

/**
 * The address worth copying from a link: a web or mail address. A link inside
 * a reply that names a project file has no address of its own (Electron hands
 * it over resolved against the app's page), so it offers none.
 */
function webLink(url: string, page: string) {
  if (!url) return undefined;
  let parsed: URL;
  try {
    parsed = new URL(url);
  } catch {
    return undefined;
  }
  if (parsed.protocol === "mailto:") return url;
  if (parsed.protocol !== "http:" && parsed.protocol !== "https:") return undefined;
  try {
    if (parsed.origin === new URL(page).origin) return undefined;
  } catch {}
  return url;
}

/** Elements described by the properties these checks read, so tests can
 * stand in for the DOM. */
type Focused = {
  tagName?: string;
  isContentEditable?: boolean;
  type?: string;
  readOnly?: boolean;
};

const notText = new Set([
  "button",
  "checkbox",
  "color",
  "file",
  "hidden",
  "image",
  "radio",
  "range",
  "reset",
  "submit",
]);

/** Whether keys typed (or text pasted) into this element edit its text. A
 * terminal's hidden textarea and an editor's contenteditable both count. */
export function isTextEntry(element: unknown): boolean {
  if (!element || typeof element !== "object") return false;
  const field = element as Focused;
  if (field.isContentEditable) return true;
  if (field.readOnly) return false;
  if (field.tagName === "TEXTAREA") return true;
  return field.tagName === "INPUT" && !notText.has(field.type ?? "text");
}

/** Whether a key belongs to an input method's composition rather than to the
 * page: Escape that cancels a candidate, Enter that confirms one. A key the
 * input method handles also reports keyCode 229. WebKit sends the key that
 * ends a composition that way after `isComposing` has gone false; Chromium
 * marks that key as composing too, so there the keyCode is only a backstop. */
export function composing(event: Pick<KeyboardEvent, "isComposing" | "keyCode">) {
  return event.isComposing || event.keyCode === 229;
}

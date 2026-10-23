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

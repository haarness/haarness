/** Elements described by the properties these checks read, so tests can
 * stand in for the DOM. */
type Focused = {
  tagName?: string;
  isContentEditable?: boolean;
  type?: string;
  readOnly?: boolean;
  value?: string;
};
type Container = { contains(node: unknown): boolean };

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

/**
 * Whether the focused element belongs to someone typing outside `dock`, so a
 * dock opening by itself must not take the focus away. The message box only
 * counts while it holds a draft: it stays in view during a question, and an
 * idle empty one would otherwise keep every answer a click away.
 */
export function typingElsewhere(
  active: unknown,
  dock: Container | null | undefined,
  composer?: unknown,
): boolean {
  if (!isTextEntry(active) || dock?.contains(active)) return false;
  return active !== composer || !!(active as Focused).value;
}

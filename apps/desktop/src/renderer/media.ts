import { attachmentMimeTypes, checkAttachments, imageType, type Attachment } from "../attachments";
import { t } from "./i18n";
export { checkAttachments, type Attachment } from "../attachments";

/**
 * Whether a dropped or pasted file goes to the draft as an attachment, by
 * the rule `fileAttachments` applies: an image by its bytes, else a media
 * type the server takes. A file named as an image that holds none counts,
 * so it is refused with its own notice. Anything else (source code, a
 * folder) is referred to by its path instead.
 */
export async function attachable(file: File) {
  if (file.type.startsWith("image/")) return true;
  if (attachmentMimeTypes.has(file.type)) return true;
  if (file.type) return false;
  // No type: an image by its bytes. A folder cannot be read.
  try {
    return !!imageType(new Uint8Array(await file.slice(0, 16).arrayBuffer()));
  } catch {
    return false;
  }
}

export async function fileAttachments(files: File[]) {
  const values: Attachment[] = [];
  if (files.reduce((total, file) => total + file.size, 0) > 20 * 1024 * 1024)
    throw new Error("Attachments exceed 20 MiB.");
  for (const file of files) {
    // An image's own bytes say what it is, whatever its name says. A file
    // that claims to be an image and is none is refused while it can still
    // be replaced; the server decodes, converts and shrinks the rest.
    const claimed = !file.type || file.type.startsWith("image/");
    const image = claimed
      ? imageType(new Uint8Array(await file.slice(0, 16).arrayBuffer()))
      : undefined;
    if (!image && file.type.startsWith("image/"))
      throw new Error(t("{name} is not an image Haarness can read.", { name: file.name }));
    const type = image ?? file.type;
    if (!attachmentMimeTypes.has(type))
      throw new Error(
        t("Attach an image, PDF, audio, or video file. To reference source code, use its workspace path."),
      );
    const raw = await new Promise<string>((resolve, reject) => {
      const reader = new FileReader();
      reader.onload = () => resolve(String(reader.result));
      reader.onerror = () => reject(new Error("Could not read " + file.name));
      reader.readAsDataURL(file);
    });
    values.push({
      name: file.name,
      data: raw.replace(
        /^data:[^;,]*(;base64,)/,
        "data:" + type + ";name=" + encodeURIComponent(file.name) + "$1",
      ),
    });
  }
  return checkAttachments(values);
}

/** Attachments read from disk by name, relabelled by what their bytes are. */
export function relabelAttachments(values: Attachment[]) {
  return values.map((value) => {
    const match = value.data.match(/^data:(image\/[^;,]+)([^,]*),(.*)$/);
    if (!match) return value;
    const head = atob(match[3].slice(0, 24));
    const type = imageType(Uint8Array.from(head, (char) => char.charCodeAt(0)));
    return type && type !== match[1]
      ? { ...value, data: "data:" + type + match[2] + "," + match[3] }
      : value;
  });
}
export function attachmentName(value: string) {
  try {
    return decodeURIComponent(
      value.match(/;name=([^;]+)/)?.[1] ?? "Attachment",
    );
  } catch {
    return "Attachment";
  }
}

import { attachmentMimeTypes, checkAttachments, type Attachment } from "../attachments";
export { checkAttachments, readAttachments, type Attachment } from "../attachments";

export async function fileAttachments(files: File[]) {
  const values: Attachment[] = [];
  if (files.reduce((total, file) => total + file.size, 0) > 20 * 1024 * 1024)
    throw new Error("Attachments exceed 20 MiB.");
  for (const file of files) {
    if (!attachmentMimeTypes.has(file.type))
      throw new Error(
        "Attach an image, PDF, audio, or video file. To reference source code, use its workspace path.",
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
        ";base64,",
        ";name=" + encodeURIComponent(file.name) + ";base64,",
      ),
    });
  }
  return checkAttachments(values);
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

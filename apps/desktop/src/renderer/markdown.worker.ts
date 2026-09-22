import { projectMarkdown, markdownPatch, type MarkdownProjection } from "./markdown-project";

const projections = new Map<number, MarkdownProjection>();
self.onmessage = (event: MessageEvent<{ key: number; revision: number; text?: string; live?: boolean }>) => {
  const { key, revision, text, live } = event.data;
  if (text === undefined) {
    projections.delete(key);
    return;
  }
  try {
    const previous = projections.get(key);
    const next = projectMarkdown(previous, text, !!live);
    projections.set(key, next);
    self.postMessage({ key, revision, patch: markdownPatch(previous, next) });
  } catch {
    projections.delete(key);
    self.postMessage({ key, revision, error: true });
  }
};

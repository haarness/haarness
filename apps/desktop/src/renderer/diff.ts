import { parsePatch, createTwoFilesPatch } from "diff";
import type { FileDiff } from "@haarness/sdk";
export interface DiffLine {
  kind: "context" | "add" | "remove" | "hunk";
  text: string;
  old?: number;
  line?: number;
}
export function patchText(file: FileDiff): string {
  return (
    file.patch ??
    createTwoFilesPatch(
      file.file,
      file.file,
      file.before ?? "",
      file.after ?? "",
    )
  );
}
export function lines(file: FileDiff): DiffLine[] {
  const patch = patchText(file);
  try {
    return parsePatch(patch).flatMap((file) =>
      file.hunks.flatMap((hunk) => {
        let old = hunk.oldStart,
          line = hunk.newStart;
        return [
          {
            kind: "hunk" as const,
            text: `@@ -${hunk.oldStart},${hunk.oldLines} +${hunk.newStart},${hunk.newLines} @@`,
          },
          ...hunk.lines
            .filter((text) => !text.startsWith("\\"))
            .map((text) =>
              text[0] === "+"
                ? { kind: "add" as const, text: text.slice(1), line: line++ }
                : text[0] === "-"
                  ? { kind: "remove" as const, text: text.slice(1), old: old++ }
                  : {
                      kind: "context" as const,
                      text: text.slice(1),
                      old: old++,
                      line: line++,
                    },
            ),
        ];
      }),
    );
  } catch {
    return [{ kind: "context", text: patch }];
  }
}
export interface SplitLine {
  left?: DiffLine;
  right?: DiffLine;
  hunk?: string;
}
export function split(rows: DiffLine[]): SplitLine[] {
  const output: SplitLine[] = [];
  for (let i = 0; i < rows.length; i++) {
    const row = rows[i];
    if (row.kind === "hunk") {
      output.push({ hunk: row.text });
      continue;
    }
    if (row.kind === "context") {
      output.push({ left: row, right: row });
      continue;
    }
    const removed: DiffLine[] = [],
      added: DiffLine[] = [];
    while (i < rows.length && rows[i].kind === "remove")
      removed.push(rows[i++]);
    while (i < rows.length && rows[i].kind === "add") added.push(rows[i++]);
    i--;
    for (let j = 0; j < Math.max(removed.length, added.length); j++)
      output.push({ left: removed[j], right: added[j] });
  }
  return output;
}

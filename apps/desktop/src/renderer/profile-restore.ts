import { t } from "./i18n";
import type { ListItem } from "./ui";

/** What `haarness profile restore --dry-run --json` says a restore would do. */
export interface RestorePlan {
  name: string;
  /** Every file the restore writes. */
  targets: string[];
  /** The targets that exist already, which the restore replaces. */
  overwrite: string[];
}

/** A profile name inside a translated sentence, isolated so a Latin name in
 * an Arabic sentence keeps its quotes and the count beside it in order. */
const isolate = (name: string) => "\u2068" + name + "\u2069";

export function restorePlan(value: unknown): RestorePlan {
  const data = (value ?? {}) as Record<string, unknown>;
  const paths = (list: unknown) =>
    Array.isArray(list)
      ? list.filter((item): item is string => typeof item === "string")
      : [];
  if (data.dryRun !== true || !Array.isArray(data.targets))
    throw new Error(t("Haarness could not preview this profile."));
  return {
    name: typeof data.name === "string" ? data.name : "",
    targets: paths(data.targets),
    overwrite: paths(data.overwrite),
  };
}

/**
 * The confirmation for a previewed restore: how many files it writes, which
 * of them it replaces, and every path, replaced ones first.
 */
export function restoreSummary(plan: RestorePlan): {
  description: string;
  list: ListItem[];
  replaces: boolean;
} {
  const count = plan.targets.length,
    replaced = plan.overwrite.length;
  const name = isolate(plan.name);
  const writes =
    count === 1
      ? t("Restoring “{name}” writes {count} file.", { name, count })
      : t("Restoring “{name}” writes {count} files.", { name, count });
  const replaces = !replaced
    ? t("No existing file is replaced.")
    : count === 1
      ? t("It already exists and will be replaced.")
      : replaced === 1
        ? t("{count} file already exists and will be replaced.", { count: replaced })
        : t("{count} files already exist and will be replaced.", { count: replaced });
  const existing = new Set(plan.overwrite);
  return {
    description: writes + " " + replaces,
    list: [
      ...plan.targets.filter((path) => existing.has(path)),
      ...plan.targets.filter((path) => !existing.has(path)),
    ].map((path) =>
      existing.has(path) ? { text: path, tag: t("Will be replaced") } : { text: path },
    ),
    replaces: replaced > 0,
  };
}

/**
 * The confirmed restore. It replaces existing files only when the preview
 * listed some: after a preview that replaced nothing, a file that appeared
 * since stops the restore instead of being overwritten. After one that did
 * replace files, any listed path is written, as the reader confirmed.
 */
export function restoreArgs(source: string[], plan: RestorePlan) {
  return [
    "profile",
    ...source,
    ...(plan.overwrite.length ? ["--force"] : []),
    "--json",
  ];
}

/** What the toast says once the restore has finished. */
export function restoredMessage(plan: RestorePlan) {
  return t("Profile “{name}” restored.", { name: isolate(plan.name) });
}

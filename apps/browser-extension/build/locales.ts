/**
 * The extension ships only the interface text it uses. Its keys are every
 * literal handed to `t("…")` or marked with `key("…")` in `src/`, plus the
 * agent cursor's labels (translated in the worker for the shared driver);
 * each language's subset is cut from the repository's shared catalogs.
 */
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";

/** The agent cursor's labels, owned by the shared driver's `agent-cursor.ts`. */
export const cursorKeys = [
  "Clicking", "Clicking “{name}”",
  "Pointing", "Pointing at “{name}”",
  "Dragging", "Dragging “{name}”",
  "Dropping", "Dropping on “{name}”",
  "Typing", "Typing in “{name}”",
  "Choosing", "Choosing in “{name}”",
  "Checking", "Checking “{name}”",
  "Unchecking", "Unchecking “{name}”",
  "Pressing {key}", "Scrolling", "Scrolling up", "Scrolling down",
];

function sources(directory: string, out: string[] = []) {
  for (const name of readdirSync(directory)) {
    const path = join(directory, name);
    if (statSync(path).isDirectory()) sources(path, out);
    else if (/\.tsx?$/.test(name)) out.push(path);
  }
  return out;
}

/** Every key the extension's source uses, in first-seen order. */
export function usedKeys(sourceDirectory: string): string[] {
  const keys = new Set<string>();
  for (const file of sources(sourceDirectory)) {
    const source = readFileSync(file, "utf8");
    for (const [, key] of source.matchAll(/\b(?:t|key)\(\s*"((?:[^"\\]|\\.)*)"/g)) keys.add(JSON.parse(`"${key}"`));
  }
  for (const key of cursorKeys) keys.add(key);
  return [...keys];
}

/** Language code → the subset of its catalog the extension uses. Throws when English lacks a key. */
export function localeSubsets(localesDirectory: string, keys: string[]) {
  const languages = JSON.parse(readFileSync(join(localesDirectory, "languages.json"), "utf8")) as { code: string }[];
  const english = JSON.parse(readFileSync(join(localesDirectory, "en.json"), "utf8")) as Record<string, string>;
  const missing = keys.filter((key) => !Object.hasOwn(english, key));
  if (missing.length) throw new Error(`Add these interface strings to locales/*.json:\n${missing.join("\n")}`);
  return Object.fromEntries(
    languages.map(({ code }) => {
      const catalog = JSON.parse(readFileSync(join(localesDirectory, `${code}.json`), "utf8")) as Record<string, string>;
      return [code, Object.fromEntries(keys.filter((key) => Object.hasOwn(catalog, key)).map((key) => [key, catalog[key]]))];
    }),
  ) as Record<string, Record<string, string>>;
}

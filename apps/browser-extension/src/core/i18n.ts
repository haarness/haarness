/**
 * Interface text. English strings are the keys, exactly as in Haarness Desktop
 * and the TUI (`locales/*.json` at the repository root); the build writes the
 * subset this extension uses to `locales/<code>.json` inside the extension.
 * The extension starts in English and never guesses the language from the
 * browser; the user picks one on the welcome page.
 */
import inventory from "../../../../locales/languages.json" with { type: "json" };

export interface Language {
  code: string;
  label: string;
  direction: "ltr" | "rtl";
}

export const languages = inventory as Language[];

export function parseLanguage(input: unknown): string {
  if (typeof input !== "string") return "en";
  const value = input.trim().replaceAll("_", "-").toLowerCase();
  if (/^zh-(hant|tw|hk|mo)(-|$)/.test(value)) return "zh-TW";
  const base = value.split("-")[0];
  const alias: Record<string, string> = { zh: "zh-CN", pt: "pt-BR", no: "nb", nn: "nb" };
  const code = alias[base] ?? base;
  return languages.find((language) => language.code.toLowerCase() === code.toLowerCase())?.code ?? "en";
}

export function direction(code: string): "ltr" | "rtl" {
  return languages.find((language) => language.code === code)?.direction ?? "ltr";
}

export type Dictionary = Record<string, string>;

/**
 * Marks an English key that is translated where it renders, so the build
 * ships it (and the catalog tests check it) though no t() call names it here.
 */
export const key = <T extends string>(value: T): T => value;

/** `{name}` placeholders are filled from `values`; unknown ones stay as written. */
export function format(text: string, values: Record<string, string | number> = {}) {
  return text.replace(/\{(\w+)\}/g, (match, name: string) => (Object.hasOwn(values, name) ? String(values[name]) : match));
}

export function lookup(dictionary: Dictionary, english: Dictionary, key: string): string {
  return (Object.hasOwn(dictionary, key) ? dictionary[key] : undefined) ??
    (Object.hasOwn(english, key) ? english[key] : undefined) ??
    key;
}

/** Reads one language's subset; a missing or broken file falls back to the English keys. */
export async function loadDictionary(code: string, read: (path: string) => Promise<unknown>): Promise<Dictionary> {
  try {
    const value = await read(`locales/${code}.json`);
    return value && typeof value === "object" ? (value as Dictionary) : {};
  } catch {
    return {};
  }
}

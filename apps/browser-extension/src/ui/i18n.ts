/**
 * Reactive `t()` for the extension's pages: English keys, the shared catalogs'
 * translations, `{name}` placeholders. Call it at render time (never in a
 * module-level constant) so a language change updates an open page.
 */
import { createSignal, type JSX } from "solid-js";
import { direction, format, loadDictionary, lookup, parseLanguage, type Dictionary } from "../core/i18n";

const read = (path: string) => fetch(chrome.runtime.getURL(path)).then((response) => response.json());
const [state, setState] = createSignal<{ code: string; dictionary: Dictionary; english: Dictionary }>({
  code: "en",
  dictionary: {},
  english: {},
});

export const language = () => state().code;

export async function applyLanguage(value: unknown) {
  const code = parseLanguage(value);
  const current = state();
  if (code === current.code && Object.keys(current.english).length) return;
  const [english, dictionary] = await Promise.all([
    Object.keys(current.english).length ? current.english : loadDictionary("en", read),
    code === "en" ? Promise.resolve({}) : loadDictionary(code, read),
  ]);
  setState({ code, dictionary, english });
  document.documentElement.lang = code;
  document.documentElement.dir = direction(code);
}

/** The language saved on the welcome page, applied before the first render to avoid a flash of English. */
export async function initialLanguage() {
  const saved = await chrome.storage.local.get("settings").catch(() => ({}) as Record<string, unknown>);
  await applyLanguage((saved.settings as { language?: string } | undefined)?.language);
}

export function t(key: string, values: Record<string, string | number> = {}): string {
  const { dictionary, english } = state();
  return format(lookup(dictionary, english, key), values);
}

/** A translated sentence around one element, wherever the language puts it. */
export function around(template: string, token: string, render: () => JSX.Element): JSX.Element {
  const [before, after = ""] = template.split(token);
  return [before, render(), after] as unknown as JSX.Element;
}

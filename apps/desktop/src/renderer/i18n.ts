import { createSignal, type JSX } from "solid-js";
import { displayLanguage, translate } from "../localization";
import { errorText, errorTextLine } from "./error-text";
export { languages, parseLanguage } from "../localization";
const [language, setLanguage] = createSignal("en");
export { language };
export function applyLanguage(value: unknown) {
  const code = displayLanguage(value);
  setLanguage(code);
  document.documentElement.lang = code;
  document.documentElement.dir = code === "ar" ? "rtl" : "ltr";
}
/** A number as the interface language writes it ("1,234", "1.234", "1 234"). */
export const formatNumber = (value: number) => new Intl.NumberFormat(language()).format(value);
/** A date and time as the interface language writes them. */
export const formatDateTime = (value: number | string | Date) => new Date(value).toLocaleString(language());
export function t(key: string, values: Record<string, string | number> = {}): string {
  return translate(language(), key).replace(/\{(\w+)\}/g, (match, name) =>
    Object.hasOwn(values, name) ? String(values[name]) : match);
}
/** A failure on one line in the reader's language, for a surface without
 * room for a title; empty for a cancellation, which shows nothing. */
export function errorLine(error: unknown) {
  const text = errorText(error);
  const said = errorTextLine(text);
  return text.known ? t(said) : said;
}

/**
 * Render a translated sentence that wraps one element, so a language can put
 * that element anywhere in the sentence instead of only where English does.
 */
export function splitAround(
  template: string,
  render: () => JSX.Element,
  token = "{project}",
): JSX.Element {
  const [before, after = ""] = template.split(token);
  return [before, render(), after] as unknown as JSX.Element;
}

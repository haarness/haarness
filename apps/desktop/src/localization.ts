// Shared by Desktop and the Rust TUI. Never infer the UI language from the OS.
import inventory from "../../../locales/languages.json" with { type: "json" };
import dict0 from "../../../locales/en.json" with { type: "json" };
import dict1 from "../../../locales/zh-CN.json" with { type: "json" };
import dict2 from "../../../locales/zh-TW.json" with { type: "json" };
import dict3 from "../../../locales/ko.json" with { type: "json" };
import dict4 from "../../../locales/de.json" with { type: "json" };
import dict5 from "../../../locales/es.json" with { type: "json" };
import dict6 from "../../../locales/fr.json" with { type: "json" };
import dict7 from "../../../locales/da.json" with { type: "json" };
import dict8 from "../../../locales/ja.json" with { type: "json" };
import dict9 from "../../../locales/pl.json" with { type: "json" };
import dict10 from "../../../locales/ru.json" with { type: "json" };
import dict11 from "../../../locales/ar.json" with { type: "json" };
import dict12 from "../../../locales/nb.json" with { type: "json" };
import dict13 from "../../../locales/pt-BR.json" with { type: "json" };
import dict14 from "../../../locales/th.json" with { type: "json" };
import dict15 from "../../../locales/bs.json" with { type: "json" };
import dict16 from "../../../locales/tr.json" with { type: "json" };
export const languages = inventory;
export const dictionaries: Record<string, Record<string, string>> = {
  "en": dict0,
  "zh-CN": dict1,
  "zh-TW": dict2,
  "ko": dict3,
  "de": dict4,
  "es": dict5,
  "fr": dict6,
  "da": dict7,
  "ja": dict8,
  "pl": dict9,
  "ru": dict10,
  "ar": dict11,
  "nb": dict12,
  "pt-BR": dict13,
  "th": dict14,
  "bs": dict15,
  "tr": dict16,
};
export function parseLanguage(input: string): string | undefined {
  const value = input.trim().split(".")[0].replaceAll("_", "-").toLowerCase();
  if (/^(zht$|zh-(hant|tw|hk|mo)(-|$))/.test(value)) return "zh-TW";
  const base = value.split("-")[0];
  const alias: Record<string, string> = { zh: "zh-CN", pt: "pt-BR", br: "pt-BR", no: "nb", nn: "nb" };
  const code = alias[base] ?? base;
  return Object.hasOwn(dictionaries, code) ? code : undefined;
}
export function displayLanguage(input: unknown): string {
  return typeof input === "string" ? parseLanguage(input) ?? "en" : "en";
}
export function translate(language: string, key: string): string {
  const dictionary = dictionaries[displayLanguage(language)];
  return (Object.hasOwn(dictionary, key) ? dictionary[key] : undefined)
    ?? (Object.hasOwn(dictionaries.en, key) ? dictionaries.en[key] : undefined) ?? key;
}

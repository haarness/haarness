import assert from "node:assert/strict";
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { cursorKeys, localeSubsets, usedKeys } from "../build/locales";
import { format, languages, lookup, parseLanguage } from "../src/core/i18n";

const src = fileURLToPath(new URL("../src", import.meta.url));
const locales = fileURLToPath(new URL("../../../locales", import.meta.url));
const catalog = (code: string) => JSON.parse(readFileSync(join(locales, `${code}.json`), "utf8")) as Record<string, string>;

function files(directory: string, out: string[] = []) {
  for (const name of readdirSync(directory)) {
    const path = join(directory, name);
    if (statSync(path).isDirectory()) files(path, out);
    else if (/\.tsx?$/.test(name)) out.push(path);
  }
  return out;
}

test("every interface string the extension uses is in all 17 shared catalogs", () => {
  const keys = usedKeys(src);
  assert.ok(keys.length > 60);
  for (const key of cursorKeys) assert.ok(keys.includes(key));
  const subsets = localeSubsets(locales, keys);
  assert.equal(Object.keys(subsets).length, 17);
  for (const { code } of languages) {
    const missing = keys.filter((key) => !Object.hasOwn(subsets[code], key) || !subsets[code][key].trim());
    assert.deepEqual(missing, [], code);
  }
});

test("translations keep every placeholder", () => {
  for (const key of usedKeys(src).filter((key) => /\{\w+\}/.test(key))) {
    const fields = key.match(/\{\w+\}/g)!.sort();
    for (const { code } of languages) assert.deepEqual(catalog(code)[key]?.match(/\{\w+\}/g)?.sort(), fields, `${code}: ${key}`);
  }
});

test("English is the default and the browser's language is never guessed", () => {
  assert.equal(parseLanguage(undefined), "en");
  assert.equal(parseLanguage("xx"), "en");
  assert.equal(parseLanguage("zh-Hant"), "zh-TW");
  assert.equal(parseLanguage("pt"), "pt-BR");
  assert.equal(parseLanguage("ar"), "ar");
  assert.equal(languages.find((language) => language.code === "ar")?.direction, "rtl");
  for (const file of files(src)) {
    const source = readFileSync(file, "utf8");
    assert.doesNotMatch(source, /navigator\.languages?\b|getUILanguage|getAcceptLanguages/, file);
  }
  assert.equal(format(lookup({}, { "{count} tabs": "{count} tabs" }, "{count} tabs"), { count: 3 }), "3 tabs");
  assert.equal(lookup({}, {}, "missing"), "missing");
});

test("pages translate at render time, never in module-level constants", () => {
  for (const file of files(join(src, "ui"))) {
    const source = readFileSync(file, "utf8");
    assert.doesNotMatch(source, /^(?:export )?const \w+\s*=\s*t\(/m, file);
  }
});

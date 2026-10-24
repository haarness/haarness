# Interface languages

The 17 interface languages of the TUI, Desktop and Haarness for Chrome: how each client picks one, what is translated, and how to add or change strings. It is for contributors and translators; the user setting is described in [Desktop settings](https://haarness.ai/docs/desktop/settings) and [Slash commands](https://haarness.ai/docs/tui/slash-commands).

Haarness starts in English. It does not infer the interface language from `LANG`, `LC_ALL`, the operating system, or the browser. An explicitly selected language takes precedence over the default.

Supported languages: English, Simplified Chinese, Traditional Chinese, Korean, German, Spanish, French, Danish, Japanese, Polish, Russian, Arabic, Norwegian, Brazilian Portuguese, Thai, Bosnian, and Turkish.

- **TUI:** `/language` opens the picker. `/language ja` selects Japanese; `/language en` restores English. The explicit saved choice in `tui-state.toml` takes precedence over `[tui].language` in the application `config.toml`. These are client preferences, independent of the attached project's server configuration.
- **Desktop:** Settings → General → Language, or `/language` to open that setting. `/language ja` changes it directly. Desktop saves the choice on this computer and shares it between its windows.
- **Haarness for Chrome:** the language choice on its welcome page (also its options page), stored in the extension's own storage; it does not follow Desktop or the TUI.
- Region spellings such as `ja_JP.UTF-8`, `zh-Hant`, `zh-TW`, `zh-HK`, `pt-BR`, and `nb-NO` are accepted for explicit selections. Unsupported stored Desktop choices fall back to English. Invalid slash-command arguments leave the current choice intact.

The JSON dictionaries are bundled into the TUI and Desktop; the Chrome extension's build writes only the keys it uses to `dist/chrome/locales/`. They cover common interface controls, navigation, General setting names, and status labels. Missing translations fall back to English; specialized settings explanations and runtime diagnostics may remain English. Assistant responses, user text, code, paths, agent/model identifiers and slash-command names are not translated. Arabic sets Desktop's interface direction to right-to-left; code and terminal surfaces preserve left-to-right ordering. Arabic glyph shaping and bidirectional display in the TUI depend on the terminal renderer.

## Maintaining translations

Add matching keys to every locale JSON file and keep `languages.json`, Rust `Language::ALL`, and Desktop's imports synchronized. Locale tests enforce dictionary key parity and the Rust/inventory language mapping; the extension's `i18n` test checks that every key it uses exists in all 17 files. English keys are stable UI text or the small existing TUI status identifiers. Read translated text at render time so a language switch updates open views without reloading. Do not translate protocol values or use translated text as persistent IDs or setting search targets.

## Third-party components

Some common translated terms include MIT-licensed contributions. The accompanying [third-party notice](THIRD-PARTY-NOTICES.md) preserves their license. Haarness bundles its dictionaries and uses them across Rust and Desktop.

## Verification

```sh
cargo test -p haarness-tui --lib
npm --prefix apps/desktop test
npm --prefix apps/browser-extension test
cargo build -p haarness-cli
python3 scripts/test-language-tmux.py
npm --prefix apps/desktop run build
HAARNESS_BIN="$PWD/target/debug/haarness" LANG=ja_JP.UTF-8 \
  npm --prefix apps/desktop run test:e2e -- --grep 'display languages default'
```

/**
 * The extension's manifest, generated at build time so its version always
 * follows the Haarness workspace version.
 */
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { manifestVersion } from "./src/core/version";

/**
 * The public half of the extension's signing key (the private half never
 * enters a repository). It pins the extension ID, which native messaging
 * allows by name, for every unpacked copy.
 */
export const EXTENSION_KEY =
  "MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEAy8ET5AuVoqeoUKD2R5YRI5tfQm+VuI8Q0LQFgmteetrPfCj3EzWsRyYzuhSxvy4Q9JVeHoB3nrfpdcFkZRYY2g+VT5RRYoIIZ2oKo/NnK7bB1LRc+kuDlv/Wtg+ziMYbW+YfFjILYuSnEHhytSBDBioIqzo2Pet0AXs+t5drwVs3ixdUlJQRy6WxlTmxSXsfmBkeV+snizCEtsORs4ahzQev9A1ImwEQE++9OW/4b6B76KGJhZsnwkuPf6FocWlRpUI+Nf6o111iBusXrgrQLYd9DpvFwkexgB0MiWI4K12UjduTy0EtEoknx9rLjtlIbZMiFsNXkG4O5iJu/Igp0QIDAQAB";
export const EXTENSION_ID = "accdjhmcgljbadpeanajllmcembhkomn";

/** Chrome's ID for a key: the first 32 hex digits of its SHA-256, spelled with a–p. */
export function extensionId(key: string) {
  const hex = createHash("sha256").update(Buffer.from(key, "base64")).digest("hex").slice(0, 32);
  return [...hex].map((digit) => String.fromCharCode(97 + Number.parseInt(digit, 16))).join("");
}

/** `[workspace.package] version` from the repository's Cargo.toml. */
export function workspaceVersion(cargoToml: string) {
  const section = /\[workspace\.package\]([\s\S]*?)(?:\n\[|$)/.exec(readFileSync(cargoToml, "utf8"))?.[1] ?? "";
  const version = /^version\s*=\s*"([^"]+)"/m.exec(section)?.[1];
  if (!version) throw new Error(`No [workspace.package] version in ${cargoToml}`);
  return version;
}

export const ICON_SIZES = [16, 32, 48, 128] as const;

export function createManifest(version: string) {
  const icons = Object.fromEntries(ICON_SIZES.map((size) => [String(size), `icons/icon-${size}.png`]));
  return {
    manifest_version: 3,
    name: "Haarness",
    description: "Let Haarness agents use Chrome — with your permission.",
    key: EXTENSION_KEY,
    version: manifestVersion(version),
    version_name: version,
    minimum_chrome_version: "125",
    // No host permissions: chrome.debugger attaches to web pages without them.
    permissions: ["debugger", "tabs", "tabGroups", "nativeMessaging", "storage", "alarms", "downloads"],
    incognito: "not_allowed",
    background: { service_worker: "background.js", type: "module" },
    action: { default_popup: "popup.html", default_title: "Haarness", default_icon: icons },
    options_page: "welcome.html",
    icons,
    commands: {
      "stop-all": {
        suggested_key: { default: "Alt+Shift+S" },
        description: "Stop Haarness in this browser",
      },
    },
  };
}

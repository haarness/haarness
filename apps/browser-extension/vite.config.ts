import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { defineConfig, type Plugin } from "vite";
import solid from "vite-plugin-solid";
import { decode, encode, resize } from "./build/png";
import { localeSubsets, usedKeys } from "./build/locales";
import { createManifest, EXTENSION_ID, EXTENSION_KEY, extensionId, ICON_SIZES, workspaceVersion } from "./manifest.config";

const here = dirname(fileURLToPath(import.meta.url));
const root = resolve(here, "../..");

/** manifest.json, the toolbar icons and the interface text, written next to the bundle. */
function extensionAssets(): Plugin {
  return {
    name: "haarness-extension-assets",
    apply: "build",
    generateBundle() {
      if (extensionId(EXTENSION_KEY) !== EXTENSION_ID) throw new Error("The manifest key does not give the pinned extension ID");
      const manifest = createManifest(workspaceVersion(resolve(root, "Cargo.toml")));
      this.emitFile({ type: "asset", fileName: "manifest.json", source: JSON.stringify(manifest, null, 2) + "\n" });
      const artwork = decode(readFileSync(resolve(root, "brand/png/app-icon-1024.png")));
      for (const size of ICON_SIZES)
        this.emitFile({ type: "asset", fileName: `icons/icon-${size}.png`, source: encode(resize(artwork, size)) });
      // The welcome page shows the mark at 48 px on high-density screens.
      this.emitFile({ type: "asset", fileName: "icons/icon-96.png", source: encode(resize(artwork, 96)) });
      const subsets = localeSubsets(resolve(root, "locales"), usedKeys(resolve(here, "src")));
      for (const [code, subset] of Object.entries(subsets))
        this.emitFile({ type: "asset", fileName: `locales/${code}.json`, source: JSON.stringify(subset) });
    },
  };
}

export default defineConfig({
  root: resolve(here, "src"),
  base: "./",
  publicDir: false,
  plugins: [solid(), extensionAssets()],
  resolve: { dedupe: ["effect", "solid-js"] },
  build: {
    outDir: resolve(here, "dist/chrome"),
    emptyOutDir: true,
    target: "chrome125",
    minify: "esbuild",
    // Extension pages allow only the extension's own scripts; nothing is inlined or preloaded.
    modulePreload: false,
    assetsInlineLimit: 0,
    rollupOptions: {
      input: {
        popup: resolve(here, "src/popup.html"),
        welcome: resolve(here, "src/welcome.html"),
        background: resolve(here, "src/background.ts"),
      },
      output: {
        entryFileNames: (chunk) => (chunk.name === "background" ? "background.js" : "assets/[name]-[hash].js"),
        chunkFileNames: "assets/[name]-[hash].js",
        assetFileNames: "assets/[name]-[hash][extname]",
      },
    },
  },
});

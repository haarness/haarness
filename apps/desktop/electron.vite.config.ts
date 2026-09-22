import { defineConfig } from "electron-vite";
import solid from "vite-plugin-solid";
import { resolve } from "node:path";
export default defineConfig({
  main: {
    build: {
      rollupOptions: {
        input: resolve("src/main/index.ts"),
        external: ["electron"],
      },
    },
  },
  preload: {
    build: {
      rollupOptions: {
        input: resolve("src/preload/index.ts"),
        output: { format: "cjs", entryFileNames: "index.cjs" },
      },
    },
  },
  renderer: {
    root: "src/renderer",
    plugins: [
      solid(),
      {
        name: "desktop-development-policy",
        apply: "serve",
        transformIndexHtml: (html) =>
          html.replace(
            "connect-src 'none'",
            "connect-src 'self' ws://localhost:* http://localhost:* ws://127.0.0.1:* http://127.0.0.1:*",
          ),
      },
    ],
    build: {
      minify: "esbuild",
      rollupOptions: {
        input: resolve("src/renderer/index.html"),
        output: {
          manualChunks(id) {
            if (
              /node_modules\/(?:@codemirror\/(?:state|view|language|commands|search|autocomplete|lint)\/|@lezer\/(?:common|highlight|lr)\/)/.test(
                id,
              )
            )
              return "editor";
            if (id.includes("node_modules/@xterm")) return "terminal";
          },
        },
      },
    },
  },
});

# Provider artwork

Monochrome SVGs are vendored from [LobeHub Icons](https://github.com/lobehub/lobe-icons) and [Models.dev](https://github.com/anomalyco/models.dev), both under MIT licenses included here. `sources.json` records the exact upstream commit URL and SHA-256 for each asset. Logos remain the property of their respective owners.

`provider-brands.ts` maps stable provider IDs and explicit aliases to filenames. `ProviderIcon.tsx` imports the files with Vite and renders them as decorative CSS masks, inheriting the current theme's text color. It never fetches assets from a configured provider or guesses a brand from its protocol/model names. Missing brands and custom providers use the neutral cloud glyph. Add a reviewed SVG and a registry entry to extend coverage; update the provenance manifest and bundled notices in `renderer/public/third-party/provider-icons` at the same time.

# Haarness brand assets

The marks the app ships. Everything here is generated; the drawings, the grid
and the reasoning live in the logo system these were chosen from.

```
svg/   the artwork — mark, wordmark, lockup, app icon, favicon
png/   raster exports, transparent, at the sizes the platforms ask for
icon.ico  Windows icon, 256 and 128, built from the PNGs above
```

## The choices

| | |
|---|---|
| **Mark** | Harness — two posts joined by a strap that overhangs them |
| **Period** | none; the H stands alone |
| **Colour** | monochrome, paper on ink and ink on paper |
| **Name** | capital initial — **Haarness.** |
| **Lockup** | the mark *is* the H, not a mark placed beside one |

The strap is the whole idea: it leaves the posts rather than joining them, so
the mark reads as something under load rather than a letter with a crossbar.

## Files

| File | Use |
|---|---|
| `svg/mark.svg` · `mark-light.svg` | the mark, for dark and light grounds |
| `svg/mark-mono.svg` | the mark in `currentColor` |
| `svg/wordmark.svg` · `-light` · `-mono` | the name alone |
| `svg/lockup.svg` · `-light` · `-mono` | the mark as the name's first letter |
| `svg/app-icon.svg` · `-light` | squircle tile, padded as macOS expects |
| `svg/favicon.svg` | tile tuned for 16px |
| `png/app-icon-*.png` | 1024, 512, 256, 128 |
| `png/mark-512.png`, `png/lockup-1600.png`, `png/wordmark-1200.png` | raster exports |
| `icon.ico` | Windows; `assets/icon.ico` in the desktop app is a copy |

## How the app uses them

`apps/desktop/src/renderer/brand.tsx` carries the same two drawings as inline
SVG so they inherit `currentColor` and scale off the font-size of whatever
they sit in. It is the only place brand artwork belongs; functional icons stay
in `icons.tsx`. The app icon is `apps/desktop/assets/icon.png`, copied from
`png/app-icon-1024.png`.

The wordmark is set in the interface face at weight 600 and −0.03em tracking —
the same values as the app's own type — and its H is redrawn at that face's cap
height, stem weight and side bearings so it belongs to the word. It is live
text rather than outlines, because the system fonts it uses are not
redistributable as outlines.

## Regenerating

The artwork comes from the logo system in `~/Desktop/haarness-logo-system`,
which holds six concepts, thirteen accents and the eight app palettes. To take
a different branch of it:

```sh
cd ~/Desktop/haarness-logo-system
python3 build.py --accent ember      # or --lowercase, or both
python3 make-index.py                # rebuild the contact sheet
```

Then copy the chosen files here and re-export the PNGs and `icon.ico`.

# Ferry mark, concept 15 "Manifest"

The single source of truth is `ferry-mark.svg`. Every other file is a
colour or layout variant of the same 24x24 shape. If the shape changes,
change it there, then regenerate the rest.

| File | Use |
|---|---|
| `ferry-mark.svg` | The mark, coloured with CSS through `currentColor`. Inline it, and colour it in the stylesheet. |
| `ferry-mark-accent.svg` | The mark in a fixed colour, `#3368A0`, for a place with no CSS. |
| `ferry-mark-white.svg` | The mark in solid white, for a dark or accent-coloured background. |
| `ferry-mark-black.svg` | The mark in solid black, for one-colour print, such as a stamp or a fax. |
| `ferry-lockup.svg` | The mark and the wordmark together, side by side. |
| `ferry-favicon.svg` | The browser tab icon. |
| `ferry-app-icon.svg` | The 1024x1024 source for the Mac and Android app icon. |

## Grid

The shape sits on a 24x24 grid. Its strokes are 2.2 units (the file
shape and the outer hull) and 2.4 units (the waterline). Do not resize a
stroke on its own. Export the mark at a multiple of 24 pixels, and the
strokes stay correct.

## Clear space

Leave one hull-height of empty space (5 units at 24 pixel scale) on all
four sides. Nothing else sits inside that space.

## Minimum sizes

- The mark alone: 20 pixels. It still reads at 16 pixels, but the folded
  corner starts to close up. Below 20 pixels, use the solid app icon
  artwork instead.
- The mark with the wordmark: 96 pixels wide. Below that, drop the
  wordmark and use the mark alone.

## Colour

The mark uses one accent colour, `#3368A0` (step 9 of the accent scale
in `design/colors.json`). The waterline track is the only part of the
mark with any transparency, at 26 percent opacity. On a background
already filled with the accent colour, the whole mark turns white,
including the track.

Never use a gradient, a second colour, a coloured shadow, or a tint
outside the accent scale.

## Platform notes

- **Web**: inline the `currentColor` version of the SVG. An `<img>` tag
  cannot take its colour from the surrounding text, so avoid it if you
  need that.
- **Mac**: import `ferry-app-icon.svg` into the app's asset catalog. The
  mark is Ferry's own brand asset, not a system SF Symbol, so do not
  swap in a system icon.
- **Android**: convert `ferry-mark.svg` with Android Studio's Vector
  Asset tool, and let the theme attribute `?attr/colorPrimary` set its
  colour.

## Wordmark

The wordmark uses Archivo Bold, with tight letter spacing
(`letter-spacing: -0.01em`) and sentence case. Archivo stands in for KH
Gortex, the typeface first specified, which could not be licensed. If KH
Gortex becomes available later, the lockup needs regenerating with it.

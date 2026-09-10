# Colour

`colors.json` is the single source of truth for colour in both apps. The Mac
app and the phone app each generate their own code from this one file. Nobody
types a hex value into a view.

The file holds one twelve-step accent scale and one twelve-step gray scale,
for light mode and for dark mode. It also holds the default background hex
for each mode. All values came from the Radix Colors custom palette tool, not
from hand-picked hex codes.

## How to regenerate it

1. Open `https://www.radix-ui.com/colors/custom`.
2. Enter `#3368A0` as the accent colour. This is the one accent for the
   product, set out in `docs/design.md`.
3. Leave gray on its automatic setting. Leave background at its default for
   each mode.
4. Read the light scale and the dark scale, twelve steps each, plus the two
   background values.
5. Update `colors.json` with the new values and a new `generated_on` date.

Do this again only if the accent colour itself changes. A change to `docs/design.md`
is what would trigger that.

## What each step is for

This follows the Radix Colors documentation at
`https://www.radix-ui.com/colors/docs/palette-composition/understanding-the-scale`.

- **Steps 1-2, backgrounds.** The app background and other subtle backgrounds,
  such as a card or a sidebar.
- **Steps 3-5, component backgrounds.** Step 3 is a component's normal state.
  Step 4 is its hover state. Step 5 is its pressed or selected state.
- **Steps 6-8, borders.** Step 6 is a subtle border on a static element, such
  as a card or a separator. Step 7 is a subtle border on an interactive
  element. Step 8 is a stronger border, used for focus rings too.
- **Steps 9-10, solid accent.** Step 9 is the purest, most saturated step in
  the scale. It is the solid accent colour itself. Step 10 is its hover state.
- **Steps 11-12, text.** Step 11 is low-contrast text. Step 12 is
  high-contrast text. Both are checked for contrast against a step 2
  background from the same scale.

## Tokens

`tokens.json` is the single source of truth for everything else in the
design system: type, spacing, radius, semantic colour roles, and icons. It
sits on top of `colors.json`. A colour role in `tokens.json` does not hold a
hex value. It names a scale and a step in `colors.json`, such as `gray` step
`1` for `background`. `on_accent` is the one exception, because white is a
literal, not a step on either scale.

Each type role, such as `body` or `caption`, names the matching text style
on each platform: a SwiftUI text style on macOS, a Material 3 typography
name on Android. It also names a weight and says what the role is for, so a
person choosing a style for new text can read the intent, not just the
name.

The icon table is the same shape as the type table. It maps one semantic
name, such as `paired` or `transfer`, to the symbol on each platform: SF
Symbols on macOS, Material Symbols on Android.

### The generator

`scripts/gen_tokens.py` reads `tokens.json` and `colors.json` and writes two
files:

- `macos/Ferry/Generated/Tokens.swift`
- `android/app/src/main/kotlin/app/ferry/Tokens.kt`

Both files carry a comment at the top saying they are generated and must
not be hand-edited. To change a token, change `tokens.json` or
`colors.json`, then run:

```sh
python3 scripts/gen_tokens.py
```

Run it with `--check` to verify the two generated files already match what
the script would write, without changing them. It exits 1 and names the
file if either one is out of date. This is the gate that stops a hand edit
to generated code from passing review:

```sh
python3 scripts/gen_tokens.py --check
```

The script uses only the Python standard library, so it needs no setup
beyond Python 3. Given the same two JSON files, it always writes the same
bytes.

## Errors

`errors.json` is the single source of truth for every error Ferry can show.
Each row holds three parts, in order: what stopped, why, what to do. Words
follow `docs/voice.md`.

### The generator

`scripts/gen_errors.py` reads `errors.json` and writes two files:

- `macos/Ferry/Generated/Errors.swift`
- `android/app/src/main/kotlin/app/ferry/Errors.kt`

Both files carry a comment at the top saying they are generated and must
not be hand-edited. To change an error's words, change `errors.json`, then
run:

```sh
python3 scripts/gen_errors.py
```

Run it with `--check` to verify the two generated files already match what
the script would write, without changing them. It exits 1 and names the
file if either one is out of date:

```sh
python3 scripts/gen_errors.py --check
```

A test in `ferry-core`, `errors_have_words`, checks that every error
variant in `ferry-core` and every `Runtime::` code in `ferry-runtime` has a
row, and that no row breaks `docs/voice.md`.

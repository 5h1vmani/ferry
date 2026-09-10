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

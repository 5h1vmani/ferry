# The UX plan

Seven steps, in order. Each one is small. The order matters, because every
step makes the next one cheaper to change. Status is kept here, so the plan
survives any one conversation.

| Step | What | Who | Status |
|---|---|---|---|
| 1 | An outcome metric on every job in `docs/jobs.md`. These are the acceptance criteria for phase 1. | Judgment | Done, 10 September 2026 |
| 2 | Information architecture in `docs/ia.md`: the objects, the navigation on each platform, the states of every screen, and the first run. | Judgment | Done, 10 September 2026 |
| 3 | The token layer. `design/tokens.json` holds spacing, radius, semantic type styles, and semantic colour roles on top of `design/colors.json`. A script generates `Tokens.swift` and `Tokens.kt`. | Mechanical, delegated | Done, 10 September 2026. Swift typechecked; Kotlin not yet compiled. |
| 4 | The component inventory: the custom views that appear on more than one screen, each with its states. About six. | Judgment | Done, 10 September 2026, in `docs/components.md` |
| 5 | The error table: every error the core can emit, mapped to what stopped, why, and what to do. One file, both apps read it. A test checks no cell is empty. | Content | Done, 10 September 2026. 89 rows. The test caught a missing row on its first run. |
| 6 | Low-fi wireframes of the four screens in their key states. Boxes and real copy. No colour, no polish. Reviewed by Shiva, changed while cheap. | Judgment, then review | Not started |
| 7 | Build the screens natively. No tests on screens until they have settled. | Mechanical, delegated, reviewed | Both apps wired to the engine and building, 10 September 2026. Sample state removed. Not yet run on a device; see `docs/manual-checks.md` task 3. Step 6 was skipped in favour of viewing the real screens; revisit if the structure needs to change. |

## Decisions already made

- Native controls on each platform. Apple's Human Interface Guidelines on the
  Mac, Material 3 on the phone. See `docs/design.md`.
- One accent, `#3368A0`, with a generated scale in `design/colors.json`.
- The typeface is a token. It defaults to the system font. HK Grotesk is a
  candidate to be judged by viewing both, not by argument. Its licence must be
  checked before it is bundled.
- The voice is `docs/voice.md`. Every string follows it.
- Atomic design's five tiers are not used. The component inventory keeps the
  useful part, which is building each shared view once.
- Formal JTBD research is not done. There is one user.

## What the UX work found that the protocol did not have

The information architecture in step 2 found a gap. A device's name has to
travel inside the encrypted channel after pairing, because the mDNS name is
deliberately random and the adb serial is not a name. The protocol has no
message for that yet. It is recorded in `docs/ia.md` and in `PLAN.md` as
phase 1 work.

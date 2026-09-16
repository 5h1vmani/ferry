# 11. Gestures, not a file manager

Date: 16 September 2026.
Status: accepted.

## Context

The engine moves files with a verified, resumable transfer. The apps
exposed it through one surface, a one-level file list on the Mac. Finder
and the phone's Files app, the surfaces people use, bypass it and hit raw
file operations. The phone could not start a transfer.

The information architecture that produced this came from an agent design
pass on 10 September 2026. Three of its rules blocked the fix:
`docs/design.md` said "four screens on each platform, and no more", the
handoff said the Mac's Files section "stays" until the mount ships, and the
phone got no way to send. The one human review step was skipped.

## Decision

The OS's own gestures drive the engine. Share on the phone, drop and a
Finder service on the Mac, and notifications on both. Ferry builds no file
browser on either platform. The Mac's Files section is removed once the
drop and the service exist.

"Four screens and no more" is withdrawn. A share target, a drop target, a
Services entry, and a notification are not screens, and the count was an
assertion with no reason behind it.

The parts of the design pass that carry a reason stay: native controls, one
accent, system fonts, the voice rules, and the three-part error.

## Consequences

- Every transfer a person starts goes through `push_files`, `pull`, or
  `pull_folder`, so every one resumes and verifies.
- A Finder copy still does not resume. The service is the door for a large
  file. Resume inside one Finder copy is a bridge change for later.
- A dropped folder is refused until the engine has `push_folder`.
- The plan and its status are in `docs/ux-fix-plan.md`.

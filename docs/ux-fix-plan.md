# The UX fix plan

Date: 16 September 2026. The maintainer accepted the assessment and this
plan on that day. Status is kept in the table at the end.

## Why

The engine's strongest property is a verified transfer that resumes after
any cut. Only one surface reached it: the Files section on the Mac, a
one-level list with a Copy button. A Finder copy does not resume, and the
bridge says so at `crates/ferry-runtime/src/dav/put.rs:24`. A copy in the
phone's Files app lands as a plain write at
`FerryDocumentsProvider.kt:295`, with no progress, retry, or resume. The
phone could not start a transfer at all: `push`, `push_files`, `pull`, and
`pull_folder` had no caller under `android/app`.

The information architecture came from an agent design pass on 10 September
2026. Its one human checkpoint, a review of the result before it was built,
was skipped. No designed screen has run on a real device. The decisions
this plan reverses are recorded in
`docs/decisions/0011-gestures-not-a-file-manager.md`.

## The principle

Route the gestures the OS already gives people into the transfer engine.
Build no file manager. The three gestures are share, drop, and notify.

## Items

### 1. The phone shares to the Mac

`MainActivity` gains intent filters for `ACTION_SEND` and
`ACTION_SEND_MULTIPLE` for any type. Shared content becomes one call to
`push_files` to the paired Mac, into the landing folder named in
`docs/engine-contract.md`, item 5. The Devices screen gains a "Send files"
control that opens the system picker into the same call. With one Mac
paired there is no device step. With no Mac paired, the share opens Devices
in its empty state.

The engine takes absolute paths. A content URI is resolved to a path when
its provider is MediaStore and the path reads with the access Ferry already
holds. Otherwise the content is copied into the app's cache directory and
pushed from there. A cache copy is deleted when the transfer whose `source`
is that path reaches Done, and at app start when no transfer names it.

### 2. Transfers notify on both sides

The engine's four events fire at most every 250 ms. That is enough.

The phone gains a notification channel `transfers`. One notification per
batch, or per single transfer, shows a progress bar while it runs, then
"Done", or "Failed" with a Retry action. `ReachableService` posts it,
because it holds the engine while the app is in the background. The words
are the ones `TransferRow` already uses.

The Mac asks for notification permission the first time a transfer starts,
and posts one notification when a batch or single transfer ends Done or
Failed. The Dock badge shows the count of running transfers and clears at
zero. The menu bar dropdown shows one line per running batch: label,
progress, speed.

### 3. The Mac accepts drops and a Finder service

A file dropped on a device row or on the detail pane calls `push_files`
into the phone's landing folder. A file dropped on the Dock icon does the
same; `CFBundleDocumentTypes` declares `public.item` with handler rank
`None`, so Ferry never becomes a default opener. A dropped folder is
refused with a three-part error, because the engine has no `push_folder`
yet. That gap is recorded below.

One Finder Services entry, "Send with Ferry", takes file URLs. A URL under
a device's mount path maps back to a remote path by removing the mount
path, and calls `pull` for a file or `pull_folder` for a folder. Any other
URL calls `push_files`. This gives the resumable engine a door inside
Finder for large files and whole folders.

Which device: the only paired one. When more than one is paired, the one
selected in the window, else the first reachable one.

### 4. The Mac's Files section is removed

Its stated reason was "the only way to fetch one named file before the
mount ships". The mount shipped. Finder browses, the drop sends, and the
service pulls. `DeviceDetail` keeps Access, Automatic, Transfers, Access
log, and Info, and gains one caption while the phone is reachable: where a
drop lands. `docs/ia.md`, `docs/components.md`, `docs/design.md`, and
`docs/manual-checks.md` lose the section.

### 5. Context menus and keyboard shortcuts on the Mac

A device row offers Open in Finder while mounted, Send files, and Forget.
A transfer row offers Retry while failed and Reveal in Finder for a
finished pull. Cmd+O opens the send panel for the selected device. Cmd+R
retries every failed transfer of the selected device.

### 6. The maintainer looks

After items 1 to 5 build, the maintainer runs `docs/manual-checks.md`
tasks 3, 4, and the new task 7 for these gestures. No visual polish before
that.

## Rules that live in the contract

Where a gesture-started push lands, and the mkdir rule, are in
`docs/engine-contract.md`, item 5, under "Where a push lands".

## The three alerts

1. `AccessLogSection.swift` had a "Reveal in Finder" control whose path was
   never passed. The control and its parameter are removed, and the IA line
   that promised it is removed with them. The log is an engine store, not a
   file a person opens.
2. Three comments described code that no longer exists. Each now states the
   current fact: `access.rs` is wired into the engine, `Snapshot.swift`'s
   mount is reported by item 6, and the phone scans and calls
   `offer_scanned` rather than `start_pairing_with(Qr)`. The bindings are
   regenerated, because the last one is a generated comment.
3. The design pass that set the UX lived only in the ignored `design_ouput/`
   folder. Its reasoning documents and the canvas moved into `docs/design-pass/`
   on that day, and were later removed once this plan closed them out; git
   history keeps them. The code copies and the design-system package are
   not kept: the repo's own `design/` folder is the source of tokens.

## Not in this pass

- A Quick Settings tile for advertising on the phone.
- `push_folder` in the engine, so a dropped folder lands as one batch.
- A menu-bar-only Mac app. Decide after the maintainer has seen the window.
- Resume inside one Finder copy. That is a bridge change, not a screen.

## Batches

| Batch | Where | Holds | Gate |
|---|---|---|---|
| 0 | main | This plan, the decision record, the design-pass record, the contract rule, the Rust comments, the bindings | `gen`, `rust-quick` |
| M | worktree, branch `ux-mac` | Items 2 (Mac), 3, 4, 5, and alerts 1 and 2 (Swift) | `mac` after each item |
| P | worktree, branch `ux-phone` | Items 1 and 2 (phone) | `android` after each item |
| audit | read only | Path handling in the share target and the service, findings to `docs/audits/ux-gestures.md` | none |
| merge | main | Both branches, then one `all` | `all` |

## Status

| Item | Status |
|---|---|
| Batch 0 | Done, 16 September 2026, commits c927e33 and 9017ff9 |
| 1. Phone share target | Built, 16 September 2026, branch `ux-phone` merged. Not run on a device. |
| 2. Notifications | Built on both, 16 September 2026. Not run on a device. |
| 3. Mac drop and service | Built, 16 September 2026, branch `ux-mac` merged. Not run on a device. |
| 4. Files section removed | Done, 16 September 2026. |
| 5. Menus and shortcuts | Built, 16 September 2026. Not run on a device. |
| 6. The maintainer looks | Ready. The audit's 17 findings are fixed and merged. The steps are `docs/manual-checks.md` task 7. |

The audit of the merged range is `docs/audits/ux-gestures.md`, with its fix pass recorded at the end.

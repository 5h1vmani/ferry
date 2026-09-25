# Open-source readiness audit

Date: 25 September 2026. Commit: de372e3.
Scope: licences, the name, public docs, history, project files, and GitHub settings.
Other audits cover capability, security bugs, visual design, and the fresh-clone build.

## Verdict

Ferry can go public after four blockers, and none of them needs a code change.
The history holds no secrets and its commit messages read well, so publish it unsquashed once the owner decides about the commit email.
For a stranger, the largest gaps are a README that hides the untested status and no private security contact.

## Blockers before going public

1. The commit email becomes public for good. All 528 commits carry the owner's personal email address (oss_scan.txt), but the GitHub profile hides its email (`gh api user` returns `"email":null`). Fix: answer Decision 1 before the first public push.
2. Nobody can report a vulnerability privately. Ferry listens on every interface (crates/ferry-runtime/src/engine/api/lifecycle.rs:217-221) and can serve all phone storage (android/app/src/main/AndroidManifest.xml:16). Fix: add SECURITY.md and turn on GitHub private vulnerability reporting at publish.
3. A third-party file with no known licence would ship under MIT. docs/design-pass/2026-09-10/support.js is a 69 KB generated bundle whose header names "dc-runtime" and carries no licence. Fix: confirm the tool's terms allow redistribution; if not, remove it from HEAD and from history in the Decision 1 rewrite.
4. The README hides that most features are untested. README.md:9-28 is a dated work log, and "None of it has run on real devices yet" sits at lines 25-26. Fix: replace it with a short status that says experimental and lists what ran on devices. Name the hardware: Apple silicon, macOS 14, and arm64 Android 12 or later.

The hardware facts come from macos/project.yml:13 and :69, and android/app/build.gradle.kts:14 and :24.
GitHub private vulnerability reporting is a repo setting that lets a reporter send a security issue that only the maintainer can read.

## Should do before going public

- A stranger's Mac build fails on the owner's Apple team (macos/project.yml:60-62). Move `DEVELOPMENT_TEAM` into an untracked local `.xcconfig` and default to ad-hoc signing. A team ID is not secret, so this is a build fix, not a leak.
- Preview data names the owner's Mac account (macos/Ferry/Support/PreviewData.swift:191-209). Use `NSHomeDirectory()` or `/Users/you` there.
- docs/design-pass/ is superseded and has 18 links to a `docs-v2/` folder that does not exist. Its HTML cannot render, because `_ds/` is not tracked ("Ferry IA.dc.html":12-20). Delete the folder from HEAD and cite its last commit in docs/ux-fix-plan.md:118.
- docs/agent-runs.md describes orchestrator and builder roles that a contributor never has. Move rules 1-4, 8, 9, and 14-16 into CONTRIBUTING.md, drop the rest, and update its 15 code references with sed.
- The only build guide is docs/manual-checks.md task 3, written as requests to one person (line 10). Move the build steps into a README "Build" section and keep the rest as a device checklist.
- The Android app bundles a proprietary Google library. `com.google.mlkit:barcode-scanning` 17.3.0 is under the "ML Kit Terms of Service" (its POM). ML Kit `common` 18.11.0 pulls in Google's `datatransport` log uploader (its POM). Say this in the README, and check Google's ML Kit data disclosure before any "nothing leaves your network" claim.
- Bug reports are useless without device facts. Add `.github/ISSUE_TEMPLATE/bug.yml` that asks for Mac model, macOS version, phone, Android version, and transport. Add `config.yml` that routes security reports to the private form.
- Set the description and topics at publish. Description: "Move files between a Mac and an Android phone over Wi-Fi or USB. Browse the phone in Finder." Topics: rust, android, macos, file-transfer, webdav, noise-protocol, mdns, adb, swiftui, jetpack-compose, uniffi.
- Protect `main` with a ruleset at publish. Block force pushes and branch deletion. Require the `rust core` and `dependency advisories` checks, with the owner on the bypass list. Do not require reviews, because one maintainer cannot approve his own pull requests.

Ad-hoc signing means a signature with no Apple identity (`codesign -s -`).

## Can wait

- The crate name `ferry-core` on crates.io belongs to another file-transfer project, FileFerry (v0.1.20, May 2026). Add `publish = false` to both crate manifests.
- An Android CI job needs its own JDK, SDK, and NDK setup, because scripts/env.sh:15-16 reads Homebrew paths. Add it after the Mac job. Linux minutes are free on public repos.
- Binary releases must carry the licence texts of MIT, Apache-2.0, ISC, and MPL-2.0 code. Generate a notices file with `cargo-about` and the Gradle `oss-licenses` plugin before the first binary.
- The uniffi-generated files come from MPL-2.0 templates (macos/Ferry/Generated/, android/app/src/main/kotlin/uniffi/ferry_runtime/ferry_runtime.kt). Name uniffi and MPL-2.0 in the notices file.
- If F-Droid or a fully open build matters, replace ML Kit with ZXing, which is Apache-2.0.
- Add CODE_OF_CONDUCT.md at the first outside contribution. Add CHANGELOG.md at the first tag.
- Add `.github/dependabot.yml` for cargo, gradle, and github-actions. Use a monthly schedule to limit pull-request noise.
- Add `permissions: contents: read` at the top of .github/workflows/ci.yml.
- The README decision list stops at 0009 (README.md:89). Add 0010 and 0011.
- README.md:37 says no free tool shows an Android phone inside Finder. DroidMac advertises itself as free, and I don't know whether it mounts in Finder. Verify the claim before any launch post.
- Commits say "Shiv Padakanti", but LICENSE:3 and Cargo.toml say "Shiva Padakanti". Pick one name if the history is rewritten anyway.
- Add docs/audits/README.md. It should say what each audit covered, and that "Fable" and "Sonnet" name the AI models that ran the reviews.
- Add a README screenshot and a 1280 by 640 social preview image after the device checks. The repo tracks no images today.

## Docs keep/rewrite/move/delete table

| File | Verdict | Reason |
|---|---|---|
| PLAN.md | Keep | It states the goal and the cut list honestly (PLAN.md:14-17). Its header already says the status line is stale. |
| docs/protocol.md | Keep | It is written so someone could build a second implementation. It is the strongest public document. |
| docs/decisions/0001 to 0011 (11 files) | Keep | They are short, dated decision records, and the README links them. |
| docs/engine-contract.md | Keep, fix line 4 | 448 code comments cite it. Line 4 names `docs-v2/contract.md`, which does not exist. |
| docs/design.md, components.md, ia.md, jobs.md, voice.md | Keep | They are the current design sources that both apps follow. |
| docs/spike-0-findings.md | Keep | It is the evidence for decision 0008, and three code files cite it. |
| docs/toolchain.md | Keep | It lists setup steps for a new machine. The fresh-clone audit checks their accuracy. |
| docs/manual-checks.md | Rewrite | It holds the only build steps, written as requests to the owner (line 10). |
| docs/ux-fix-plan.md | Keep, small rewrite | 47 code comments cite it. Replace the owner's name at lines 3, 95, 97, 125, and 148 with "the maintainer". |
| docs/ux-plan.md | Delete | It is a process status table that ux-fix-plan.md supersedes. No code cites it. |
| docs/agent-runs.md | Rewrite into CONTRIBUTING.md | Its test rules help contributors. Its roles and prompt sections describe a process a stranger cannot join. |
| docs/audits/ (all 12 files) | Keep, add an index | 145 code comments cite them, and they show adversarial review. A stranger cannot tell what "fable" means without an index. |
| docs/audits/oss-readiness.md (this file) | Move out or delete after acting | It is a to-do list for the owner, not a record a stranger needs. |
| docs/design-pass/README.md, 2026-09-10/{ia,jobs,components,contract,handoff}.md, 2026-09-11/kotlin-README.md | Delete | They are superseded copies with 18 dead `docs-v2/` links. Git history keeps them. |
| docs/design-pass/2026-09-10/Ferry IA.dc.html and support.js | Delete | The page cannot render without `_ds/`. It holds /Users/<owner> paths, and support.js has no known licence. |
| spike/README.md, spike/webdav-probe/{Cargo.toml, Cargo.lock, src/main.rs} | Keep | The spike is small, labelled as throwaway, and cited as evidence. |
| scripts/env.sh, gen_bindings.sh, gen_common.py, gen_errors.py, gen_tokens.py | Keep | They are the build and generator tools. |
| scripts/gate.sh | Keep, one comment | Line 3 says "every builder and the orchestrator". Say "every contributor and CI" instead. |

Commit messages read well to a stranger, and they need no rewrite.
I sampled 41 subjects, one in every 13 commits.
They are imperative and plain, and the median subject is 58 characters long.
21 subjects start with an audit ID such as "H7:", which only the audit file explains.
9 subjects are bare "Merge branch" lines, and 49 subjects are longer than 72 characters.
453 commits carry a `Co-Authored-By` trailer that names a Claude model, which is a normal, open practice.

## Licence table

`cargo metadata --format-version 1 --locked --offline` ran without the network (exit 0).
Cargo.lock holds 173 packages across all targets, including Ferry's own two MIT crates.
162 of them are MIT, Apache-2.0, or BSD, or offer a choice that includes one of those.
spike/webdav-probe has no dependencies.

| Package | Version | Licence | Where it ends up | Fine under an MIT release? |
|---|---|---|---|---|
| uniffi, uniffi_core, uniffi_macros, uniffi_meta, uniffi_pipeline, uniffi_udl, uniffi_internal_macros, uniffi_bindgen | 0.32.1 | MPL-2.0 | uniffi_core links into both apps; bindgen runs at build time | Yes. MPL-2.0 applies per file, so only changes to uniffi's own files must be published. Ferry changes none. |
| ring | 0.17.14 | Apache-2.0 AND ISC | Linked, through snow | Yes. Both are permissive. Ship both texts with binaries. |
| untrusted | 0.9.0 | ISC | Linked, through ring | Yes. ISC is permissive. |
| unicode-ident | 1.0.24 | (MIT OR Apache-2.0) AND Unicode-3.0 | Build time only | Yes. Unicode-3.0 is permissive. |
| com.google.mlkit barcode-scanning, common, vision-common, barcode-scanning-common; com.google.android.gms play-services-mlkit-barcode-scanning | 17.3.0, 18.11.0, 17.3.0, 17.0.0, 18.3.1 | ML Kit Terms of Service (proprietary) | Android APK | Yes for source and for APKs on GitHub. It is not open source, so F-Droid would refuse the app. |
| com.google.android.gms play-services-base, play-services-basement, play-services-tasks | 18.5.0, 18.4.0, 18.2.0 | Android Software Development Kit License (proprietary) | Android APK, through ML Kit | Same as the ML Kit row. |
| net.java.dev.jna jna | 5.19.1 | LGPL-2.1-or-later OR Apache-2.0 | Android APK | Yes. Take it under Apache-2.0. |

The other Android libraries are Apache-2.0.
I read the POMs of camera-core, material-icons-extended, kotlinx-coroutines-android, firebase-components, firebase-encoders-json, and transport-backend-cct.
The rest come from the same AndroidX and JetBrains families, which publish under Apache-2.0.
AGP, the Kotlin compiler, and the Gradle wrapper run at build time only and do not ship.
The Mac app uses no Swift packages, because macos/project.yml has no `packages` key.
The repo tracks no fonts and no images.
The Android launcher icon is an in-repo vector drawn for Ferry (android/app/src/main/res/drawable/ic_launcher_foreground.xml:2-3).
The Mac app has no icon yet.
docs/ux-plan.md:22 names HK Grotesk as a candidate font, but nothing bundles it.

## Decisions for the owner

1. **Rewrite all 528 commits to the GitHub noreply address before the first public push?**
   Recommendation: yes, because your profile already hides the address and a public commit email cannot be withdrawn.
   The address is `65507531+5h1vmani@users.noreply.github.com` (from `gh api user`).
   Use `git filter-repo --mailmap`, and force-push the private remote once, which only you can approve.
   Every hash changes, so add one commit on top that remaps the 55 old hashes cited in docs and tests.
   Then set `user.email` for this repo to the noreply address.

2. **Publish a squashed fresh history instead?**
   Recommendation: no, because the 528 commits show how the engineering was done, and gitleaks found nothing to hide.
   A squash would also break the 55 commit hashes that docs and tests cite.

3. **Keep the name "Ferry"?**
   Recommendation: yes for the repo, because the collisions found are adjacent products and no trademark record turned up.
   MAMP publishes a Mac file-transfer app named Ferry, in public preview (mamp.info/ferry).
   The crates.io name `ferry` is a file-mover CLI, and `ferry-core` belongs to FileFerry.
   Several GitHub LAN file tools also use the name, for example strix52/ferry and firaslamouchi21/Ferry.
   My three searches found no trademark record, but they did not query USPTO or EUIPO.
   Check both registries before any store listing or notarized release.

4. **Keep `app.ferry` as the Mac bundle id and the Android applicationId?**
   Recommendation: no, unless you own ferry.app, because the Play applicationId can never change after the first upload.
   Someone has registered ferry.app: it resolves to 44.232.173.249 and uses spaceship.net name servers. Whois did not name the owner, so I don't know who it is.
   Neither Apple nor Google checks domain ownership, so builds and notarization still work.
   The first account that uploads `app.ferry` to Play, or registers it with Apple, owns that id.
   Pick an id under a domain you control (android/app/build.gradle.kts:13, macos/project.yml:6 and :55).
   `io.github.5h1vmani.ferry` is not valid on Android, because every segment must start with a letter.
   The Kotlin `namespace` (android/app/build.gradle.kts:9) can stay as it is.

5. **Run the Mac build in CI once the repo is public?**
   Recommendation: yes, because GitHub-hosted runners, macOS included, cost nothing on public repositories.
   GitHub's billing docs say usage is free "for public repositories that use standard GitHub-hosted runners".
   For reference, a private repo pays 0.062 US dollars per macOS minute beyond its quota.
   This reopens PLAN.md:367-369, whose cost reason ends at publish.
   The job runs `brew install xcodegen`, then `scripts/gate.sh mac` with code signing turned off, because project.yml:60-62 needs your certificate.
   It catches a Rust API change that breaks the Swift app, which the `gen` gate cannot see.

6. **Attach binaries to the first public release?**
   Recommendation: no, because the later features have not run on a device (README.md:25-26).
   Publish source only until docs/manual-checks.md tasks 4 to 7 pass.
   The first binary release is then an ad-hoc signed zip for Apple silicon and an APK signed with a dedicated release key.
   Add SHA-256 checksums and the licence notices file to that release.
   A notarized DMG can wait, because it needs the 99 US dollar Apple Developer Program (PLAN.md:275).
   A debug APK is wrong, because the release build uses the debug key (android/app/build.gradle.kts:32).
   That key differs per machine, so a new build would not install over the old one.
   Check Google's Android developer verification rules before the first APK; I have not verified their current state.
   A Play listing would also need a declaration for MANAGE_EXTERNAL_STORAGE (AndroidManifest.xml:16).

7. **Turn on GitHub Discussions?**
   Recommendation: no, because one maintainer should watch one channel, and issues cover questions at this size.
   Keep the wiki off as well.

## Sources

- MAMP Ferry: https://www.mamp.info/ferry/
- crates.io: https://crates.io/api/v1/crates/ferry and https://crates.io/api/v1/crates/ferry-core
- GitHub Actions billing: https://docs.github.com/en/billing/concepts/product-billing/github-actions
- GitHub projects named ferry: https://github.com/strix52/ferry and https://github.com/firaslamouchi21/Ferry
- DroidMac: https://www.droidmac.com/

# Checks that need a person at the keyboard

Both questions that needed a person are now settled or nearly so. Task 1 is
answered and recorded below. Task 2 is a one minute confirmation.

A third task about FSKit used to sit here. It is answered and removed. Public
bug reports run to June 2026 and no source confirms a fix, so the Finder mount
stays on WebDAV. See decision record 8 for the sources.

For each one, copy what you see and send it back. There is a short "what to
send me" list at the end of every task.

---

## Task 1: does USB tethering give the Mac a network connection?

**Answered on 10 September 2026. No.**

The phone is a Pixel 3 XL on Android 12. Android 12 tethers over RNDIS, and
macOS has no RNDIS driver, so no network port appears. Newer Android versions
tether over NCM, which macOS can drive, so a newer phone might pass. This is
the phone the app is for, so the plan builds the USB path properly.

Nothing more to do here.

## Task 2: confirm which Apple team you have

**Time: about 1 minute.** This used to be a ten minute test. It shrank, because
research answered the expensive part.

### What is already answered

Apple's own capability table settles the 99 dollar question. Read on
10 September 2026 at
<https://developer.apple.com/help/account/reference/supported-capabilities-macos>.

| Thing Ferry needs | Free personal team | Cost |
|---|---|---|
| App Groups | Yes | Free |
| A real signing certificate for key storage | Yes, lasts about a year | Free |
| FileProvider Testing Mode | No | 99 US dollars a year |
| FSKit Module | No | 99 US dollars a year |

So phase 4 costs 99 dollars a year, and nothing before it costs anything.
Phases 1, 2 and 3 all run on the free account.

Worth knowing: most guides written before 2025 say a free account cannot use
App Groups at all. Apple's current table says it can. The old advice is out of
date.

### The one thing left to check

Just confirm your account really is a personal team, so we know which row of
that table you are in.

**1.** Open **Xcode**. In the menu bar click **Xcode**, then **Settings**.

**2.** Click the **Accounts** tab.

**3.** Your Apple ID appears in the list on the left. **Click it once.** This is
the step people miss.

**4.** The right side now shows a table with a **Team Name** column and a
**Role** column.

A free account shows a row whose name ends in **(Personal Team)**, with the
role **User**. A paid account shows an organisation name and the role **Agent**
or **Admin**.

**5.** Send me a screenshot of that table. Hold **Command + Shift + 4** and drag
a box around it.

### What you do not need to do now

You do not need to build a test project. You do not need to try adding App
Groups. Apple's table already tells us the answer, and your machine will create
its signing certificate on its own the first time we build the real Mac app.

There is one small unknown left, and it can wait until we build something. Apple
says a free account's provisioning profiles expire after seven days on a
device. Nobody documents whether that clock applies when the "device" is the
same Mac you are building on. We will find out when phase 1 runs, and it costs
nothing to find out then.

## Sending results back

You do not need to write anything neat. Copy and paste is fine. Screenshots are
better than typing errors out by hand.

If a step does not match what you see on screen, stop and tell me what is
different. Menus move between versions, and a wrong guess wastes more time than
a question.

If any step fails, that is still a result. A failure answers the question just
as well as a success, and I would rather know than guess.

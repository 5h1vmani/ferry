# Checks that need a person at the keyboard

One question blocks part of this project, and one is a quick confirmation.

Task 1 needs a cable plugged in. It is the one that matters, and it may remove
weeks of work. Task 2 is now a one minute check.

A third task about FSKit used to sit here. It is answered and removed. Public
bug reports run to June 2026 and no source confirms a fix, so the Finder mount
stays on WebDAV. See decision record 8 for the sources.

For each one, copy what you see and send it back. There is a short "what to
send me" list at the end of every task.

---

## Task 1: does USB tethering give the Mac a network connection?

**Time: about 5 minutes.**

**Why this matters.** The plan says building the USB path takes three to five
weeks. If the phone can already give the Mac a network connection over the
cable, most of those weeks disappear.

**Status: not answered yet.** The first attempt showed no change at all. That is
the same result you get when tethering never actually switched on, so it does
not tell us which happened. The steps below separate the two cases.

**One warning.** While tethering is on, your Mac's internet goes through the
phone's mobile data. Turn it off when you finish.

### Steps

**1.** Plug the phone into the Mac. Use a cable you know moves data, not a
charge-only one. On the phone, allow any prompt about trusting the computer,
and pick the option about **File transfer** or **Data** rather than **Charging
only**.

**2.** On the phone, open **Settings** and search for **Tethering**. On most
Android phones it sits under **Network and internet**, then **Hotspot and
tethering**. Turn on **USB tethering**.

Note what happens to that switch. There are three cases and all three matter:

- It turns on and stays on.
- It is greyed out and will not turn on.
- It turns on and then switches itself off after a second.

**3.** Wait ten seconds. Then run this on the Mac.

```bash
echo "=== 1. USB devices the Mac can see ==="; system_profiler SPUSBDataType -json 2>/dev/null | grep -i '"_name"' | sed 's/.*: //' | sort -u; echo; echo "=== 2. Network ports ==="; networksetup -listallhardwareports; echo "=== 3. Interfaces holding an address ==="; ifconfig | grep -E "^[a-z0-9]+:|inet " | grep -B1 "inet " | grep -v "^--"
```

**4.** Turn USB tethering off on the phone when you are done.

### What to send me

- Which of the three cases happened at step 2.
- The whole output of the command in step 3.
- The make and model of the phone.

### What the answer means

Section 1 of the output tells us whether the Mac sees the phone at all. If the
phone is not listed there, the cable or the USB mode is the problem, and
nothing else in the test can work.

Section 2 lists network ports by a readable name. A tethered phone usually
appears here even when `ifconfig -l` looks unchanged.

If the phone appears in section 1 but no new port appears in section 2, then
tethering genuinely does not present a network connection, and the plan stays
as it is. That is a real answer and a useful one.

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

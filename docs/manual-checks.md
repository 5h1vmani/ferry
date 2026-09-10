# Checks that need a person at the keyboard

Two questions block parts of this project. Neither can be answered by code. One
needs a cable plugged in, the other needs an Apple ID password.

Do them in this order. Task 1 is the shortest and may save the most work.

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

## Task 2: check your Apple team, then test the permissions

**Time: about 10 minutes.** You have already signed in, so start at part B.

**Why this matters.** Two things need it. It decides whether the good Finder
integration is free or costs 99 US dollars a year. It also gives the app a
stable signing certificate, which it needs to store its encryption key safely
on the Mac.

### Part A: where to see your team

**1.** Open **Xcode**. In the top menu bar click **Xcode**, then **Settings**.

**2.** Click the **Accounts** tab.

**3.** Your Apple ID appears in the list on the left. **Click it once.**

**4.** The right side of the window now shows a table with a **Team Name**
column and a **Role** column. This is what you are looking for.

A free account shows a row whose name ends in **(Personal Team)**, with the
role **User**. A paid account shows your organisation name and the role
**Agent** or **Admin**.

**5.** Take a screenshot of that table. Hold **Command + Shift + 4** and drag a
box around it.

### Important thing to know before Part B

Signing in **does not** create a signing certificate on its own. Xcode makes one
the first time you build a project that has a team selected.

So if you run a terminal command now and it says `0 valid identities found`,
nothing is wrong. Part B is what creates the certificate.

### Part B: test whether the free team can use App Groups

This is the part that answers the 99 dollar question.

**1.** In Xcode click **File**, then **New**, then **Project**. Choose **macOS**
along the top, then **App**. Click **Next**.

**2.** For **Product Name** type `EntitlementProbe`. Leave everything else
alone. Click **Next**, then save it to your **Desktop**. This gets deleted at
the end.

**3.** In the file list on the left, click the blue project icon at the very
top. In the middle panel, under **TARGETS**, click **EntitlementProbe**.

**4.** Click the **Signing & Capabilities** tab along the top.

**5.** Tick **Automatically manage signing**. In the **Team** dropdown, choose
your Personal Team.

Wait a few seconds. Xcode creates your certificate at this moment. A line
saying **Signing Certificate: Apple Development** should appear.

**6.** Click **+ Capability** near the top left of that panel. A search window
opens. Type `App Groups`. Double click **App Groups** in the results.

**7.** A new **App Groups** section appears below. Click the small **+** under
it. Type `group.com.ferry.probe` and press **Return**.

**Watch that panel now. This is the moment that answers the question.**

**8.** Take a screenshot of the whole **Signing & Capabilities** panel, whether
or not anything went red.

**9.** Press **Command + B** to build. Note whether it succeeds or fails.

**10.** Run this in Terminal. It shows whether a certificate now exists.

```bash
echo "=== signing identities ==="; security find-identity -v -p codesigning; echo "=== provisioning profiles ==="; ls ~/Library/Developer/Xcode/UserData/Provisioning\ Profiles/ 2>/dev/null | head
```

**11.** Drag the `EntitlementProbe` folder from your Desktop to the Trash.

### What to send me

- The screenshot from Part A step 5.
- The screenshot from Part B step 8.
- Any **red** text in that panel, copied out as text if you can.
- Whether the build at step 9 succeeded.
- The full output of the command at step 10.

### What the answer means

Before you started, step 10 printed `0 valid identities found` and said the
profiles folder does not exist. Any change is the result.

If App Groups was accepted with no red text, the good Finder integration is
free. If it was refused, that version costs 99 US dollars a year, and the free
version we already proved works stays as the plan.

Either way, the certificate from step 5 is what phase 1 needs for key storage.
So this task is worth doing whatever the App Groups answer turns out to be.

## Sending results back

You do not need to write anything neat. Copy and paste is fine. Screenshots are
better than typing errors out by hand.

If a step does not match what you see on screen, stop and tell me what is
different. Menus move between versions, and a wrong guess wastes more time than
a question.

If any step fails, that is still a result. A failure answers the question just
as well as a success, and I would rather know than guess.

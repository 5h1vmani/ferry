# Checks that need a person at the keyboard

Three questions block parts of this project. None of them can be answered by
code. Each needs someone to plug in a cable, type a password, or click through
Xcode.

Do them in this order. Task 1 is the shortest and may save the most work.

For each one, copy what you see and send it back. There is a short "what to
send me" list at the end of every task.

---

## Task 1: does USB tethering give the Mac a network connection?

**Time: about 5 minutes.**

**Why this matters.** The plan says building the USB path takes three to five
weeks. If the phone can already give the Mac a network connection over the
cable, then the USB path is the network code we have already written, and most
of those weeks disappear.

**One warning before you start.** While tethering is on, your Mac's internet
goes through the phone's mobile data. Turn tethering off when you finish, or
watch your data usage.

### Steps

**1.** Open the Terminal app on the Mac. Run this first, with the phone
unplugged. It lists the network connections the Mac has right now.

```bash
ifconfig -l
```

Copy the whole line it prints. That is your "before" list.

**2.** Plug the phone into the Mac with a USB cable. Use a cable you know can
move data, not a charge-only one. If a box appears on the phone asking about
trusting the computer or choosing a USB mode, allow it and choose the option
about data or file transfer.

**3.** On the phone, open **Settings**, then search for **Tethering** or
**Hotspot**. Turn on **USB tethering**. The exact menu name changes between
phones. On most it sits under Network and internet, then Hotspot and tethering.

If **USB tethering** is greyed out, the cable is probably charge-only. Try a
different cable.

**4.** Wait about ten seconds. Then run the same command again on the Mac.

```bash
ifconfig -l
```

**5.** Compare the two lines. If a new name appeared, run this to see its
details. Replace `enX` with the new name you spotted.

```bash
ifconfig enX
```

**6.** Turn USB tethering off on the phone when you are done.

### What to send me

- The "before" line from step 1.
- The "after" line from step 4.
- If a new name appeared, the full output from step 5.
- Whether **USB tethering** was greyed out or worked normally.

### What the answer means

If a new connection appeared with an address next to the word `inet`, that is
good news, and I will remove most of one phase from the plan.

If nothing new appeared, the plan stays as it is. That is a useful answer too.

---

## Task 2: sign an Apple ID into Xcode

**Time: about 10 minutes.**

**Why this matters.** Two things need this.

First, it decides whether the fancy Finder integration is free or costs 99 US
dollars a year. Second, the app needs a stable signing certificate to store its
encryption key safely on the Mac. Without one, the Mac will ask permission
every single time the app is rebuilt.

You need your Apple ID password. I cannot do this part.

### Steps

**1.** Open **Xcode**.

**2.** In the top menu bar, click **Xcode**, then **Settings**. Click the
**Accounts** tab.

**3.** Click the **+** button at the bottom left. Choose **Apple ID**. Sign in
with your Apple ID and password.

**4.** When it finishes, you should see a team listed with a name ending in
**(Personal Team)**. If you see it, this step worked.

**5.** Now make a throwaway project to test the permissions. Click **File**,
then **New**, then **Project**. Choose **macOS** at the top, then **App**.
Click **Next**.

**6.** For **Product Name** type `EntitlementProbe`. Leave everything else
alone. Click **Next**, then save it on your **Desktop**. This project gets
deleted at the end.

**7.** In the list on the left, click the blue project icon at the very top.
Then in the middle panel, click **EntitlementProbe** under **TARGETS**.

**8.** Click the **Signing & Capabilities** tab along the top.

**9.** Tick **Automatically manage signing**. In the **Team** dropdown, choose
your Personal Team.

**10.** Click **+ Capability** at the top left of that panel. A search box
opens. Type `App Groups`. Double click **App Groups** in the results.

**11.** A new **App Groups** section appears. Click the small **+** under it.
Type `group.com.ferry.probe` and press Return.

**This is the moment that answers the question.** Watch that panel closely.

**12.** Take a screenshot of the **Signing & Capabilities** panel. On a Mac,
hold **Command + Shift + 4**, then drag a box around the panel.

**13.** Now try adding the second piece. Click **File**, then **New**, then
**Target**. Choose **macOS** at the top. Look for **File Provider Extension**.

If you find it, select it, click **Next**, name it `ProbeProvider`, and click
**Finish**. If Xcode offers to activate a new scheme, click **Activate**.

If you cannot find **File Provider Extension** in that list, tell me. That
itself is an answer.

**14.** Press **Command + B** to build. Wait for it to finish.

**15.** Open Terminal and run this. It shows whether a signing certificate now
exists.

```bash
security find-identity -v -p codesigning; ls ~/Library/Developer/Xcode/UserData/Provisioning\ Profiles/ 2>/dev/null | head
```

**16.** Drag the `EntitlementProbe` folder from your Desktop to the Trash. It
has done its job.

### What to send me

- Whether a team ending in **(Personal Team)** appeared in step 4.
- The screenshot from step 12.
- Any **red** text in the Signing & Capabilities panel, copied exactly.
- Whether **File Provider Extension** existed in the list in step 13.
- Whether the build in step 14 succeeded or failed, and the first error if it
  failed.
- The full output from step 15.

### What the answer means

Before you started, step 15 printed `0 valid identities found` and said the
folder does not exist. Any change is the result.

If the App Group worked with no red text, the Finder integration is free.
If it showed an error, the good version costs 99 US dollars a year, and the
free version we already proved works stays as the plan.

---

## Task 3: does FSKit work on this version of macOS?

**Time: about 20 minutes. Do Task 2 first.**

**Why this matters.** The plan says a Mac technology called FSKit is broken. That
note is based on macOS 26.1 and 26.2. Your Mac runs 26.6.2, which is four
updates later. If FSKit works now, it is a cleaner way to show the phone in
Finder than the method we proved.

I do not know whether it works. That is exactly why this is worth 20 minutes.

### Steps

**1.** Open **Xcode**. Click **File**, then **New**, then **Project**. Choose
**macOS**, then **App**. Name it `FSKitProbe` and save it to the **Desktop**.

**2.** Set the team the same way as Task 2: click the project icon, click the
target, open **Signing & Capabilities**, tick **Automatically manage signing**,
and choose your Personal Team.

**3.** Click **File**, then **New**, then **Target**. Choose **macOS** at the
top.

**4.** In the list of target types, look for anything with **FSKit** or
**File System** in its name.

**Stop here and tell me what you find.** There are three possible answers, and
all three are useful:

- You found something with FSKit or File System in the name. Tell me its exact
  name and take a screenshot of that list.
- The list has nothing like that. Tell me, and take a screenshot of the whole
  list so I can see what is offered.
- Something else happened. Describe it.

**5.** Drag `FSKitProbe` to the Trash when you are done.

### What to send me

- A screenshot of the target list from step 4.
- The exact name of any FSKit or File System option, if one exists.

---

## Sending results back

You do not need to write anything neat. Copy and paste is fine. Screenshots are
better than typing errors out by hand.

If a step does not match what you see on screen, stop and tell me what is
different. Menus move between versions, and a wrong guess wastes more time than
a question.

If any step fails, that is still a result. A failure answers the question just
as well as a success, and I would rather know than guess.

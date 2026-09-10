# The jobs Ferry is hired for

Every screen and every feature serves one of these. A feature that serves none
of them does not get built. A screen that serves two is probably two screens.

Written as: when a situation happens, the person wants something, so that an
outcome follows.

## Job 1: use the phone's files like any other folder

**When** I come home with new photos on the phone, **I want** them to appear on
the Mac as a folder, **so that** I use them without a transfer step.

Served by: the Finder mount (phase 2), discovery, and pairing once.

**Done when:** within 30 seconds of the phone joining the Wi-Fi, its folder can
be opened in Finder with no action on either device.

## Job 2: the cable always works

**When** the Wi-Fi is slow, blocked, or absent, **I want** the cable to work
with nothing changed, **so that** a transfer never depends on which room I am
in.

Served by: the adb USB transport, and the transport indicator that says which
path is active and how fast.

**Done when:** with Wi-Fi off, plugging in the cable reaches "connected" within
10 seconds, with no action beyond plugging in.

## Job 3: a dropped transfer continues

**When** a transfer stops halfway, **I want** it to continue from where it
stopped, **so that** a bad moment does not cost the whole file.

Served by: session resume, the persisted manifest, and the temporary file that
is renamed only when it verifies.

**Done when:** after a dropped link, the transfer continues within 5 seconds of
the link returning, and refetches at most one chunk.

## Job 4: trust once, then never think about it

**When** I pair a phone, **I want** it trusted from then on and nothing else
trusted at all, **so that** security is a thing I did once rather than a thing
I do.

Served by: commit and reveal pairing, pinned keys, and forgetting a device.

**Done when:** pairing takes under 60 seconds including reading the code, and
after it there is never another prompt until the person forgets the device.

## Job 5: invisible to strangers

**When** I am on a cafe or hotel network, **I want** no stranger to learn that
my phone is there, **so that** being reachable by my Mac never means being
visible to a room.

Served by: a random mDNS name, no key material in the advertisement, a switch
to stop advertising, and a pairing mode that times out.

**Done when:** a packet capture on the network shows no device name, model,
or key material, and the advertisement stops within 2 seconds of the switch.

## Job 6: know what stopped and what to do

**When** something fails, **I want** to be told what stopped, why, and what to
do next, **so that** I never guess.

Served by: the three-part error rule in `docs/voice.md`, and a single table
that maps every core error to its words.

**Done when:** the error table has a row for every error the core can emit and
no cell is empty. A test checks this, so it cannot drift.

## Job 7: new photos reach the Mac on their own

**When** I come home with new photos on the phone, **I want** them on the Mac
without doing anything, **so that** the phone is never the only copy.

Served by: phase 2 item 2 in `PLAN.md`. Before any transfer, the Mac asks
whether it already holds the file's root hash. When the phone appears, the
Mac pulls from DCIM every file it does not hold, using MediaStore's "new since
last time". One way, phone to Mac, and additive. It never deletes and never
writes back, so it is not the two-way sync listed below.

**Done when:** a photo taken on the phone is in the Mac's Ferry folder within
one minute of the phone appearing on the network, and a photo the Mac already
holds is never copied a second time.

## Job 8: the Mac's folder inside the phone's own apps

**When** an app on the phone asks me to pick a file, **I want** the Mac's
shared folder to be one of the places, **so that** I never copy a file to the
phone first.

Served by: phase 2 item 9 in `PLAN.md`. Android's DocumentsProvider with
proxy file descriptors, over the same list, stat, read, and write operations
the Finder mount uses.

**Done when:** the Mac's shared folder appears in the Files app and in an
app's open dialog while the Mac is reachable, and a file opens from it with
no copy step.

## Jobs Ferry is not hired for

These are stated so nobody builds for them by accident.

- Sending files to a stranger's phone. That is LocalSend's job.
- Backing up the phone to the cloud.
- Keeping two folders in sync over time.
- Managing a phone that does not have Ferry installed.

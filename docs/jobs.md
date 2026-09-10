# The jobs Ferry is hired for

Every screen and every feature serves one of these. A feature that serves none
of them does not get built. A screen that serves two is probably two screens.

Written as: when a situation happens, the person wants something, so that an
outcome follows.

## Job 1: use the phone's files like any other folder

**When** I come home with new photos on the phone, **I want** them to appear on
the Mac as a folder, **so that** I use them without a transfer step.

Served by: the Finder mount (phase 2), discovery, and pairing once.

## Job 2: the cable always works

**When** the Wi-Fi is slow, blocked, or absent, **I want** the cable to work
with nothing changed, **so that** a transfer never depends on which room I am
in.

Served by: the adb USB transport, and the transport indicator that says which
path is active and how fast.

## Job 3: a dropped transfer continues

**When** a transfer stops halfway, **I want** it to continue from where it
stopped, **so that** a bad moment does not cost the whole file.

Served by: session resume, the persisted manifest, and the temporary file that
is renamed only when it verifies.

## Job 4: trust once, then never think about it

**When** I pair a phone, **I want** it trusted from then on and nothing else
trusted at all, **so that** security is a thing I did once rather than a thing
I do.

Served by: commit and reveal pairing, pinned keys, and forgetting a device.

## Job 5: invisible to strangers

**When** I am on a cafe or hotel network, **I want** no stranger to learn that
my phone is there, **so that** being reachable by my Mac never means being
visible to a room.

Served by: a random mDNS name, no key material in the advertisement, a switch
to stop advertising, and a pairing mode that times out.

## Job 6: know what stopped and what to do

**When** something fails, **I want** to be told what stopped, why, and what to
do next, **so that** I never guess.

Served by: the three-part error rule in `docs/voice.md`, and a single table
that maps every core error to its words.

## Jobs Ferry is not hired for

These are stated so nobody builds for them by accident.

- Sending files to a stranger's phone. That is LocalSend's job.
- Backing up the phone to the cloud.
- Keeping two folders in sync over time.
- Managing a phone that does not have Ferry installed.

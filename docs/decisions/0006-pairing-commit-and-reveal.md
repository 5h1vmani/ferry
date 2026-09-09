# 6. Commit and reveal during pairing

Date: 9 September 2026.
Status: accepted.

## Context

The first design paired two devices with a Noise `XX` handshake and a short
code derived from both static public keys. The user compared the code on two
screens.

That design is broken against an active attacker.

In Noise `XX` the initiator sends its static public key in message three. By
then it has seen the responder's ephemeral key and static key. An attacker in
the middle runs two handshakes at once. Towards the phone it acts as the
initiator, so it can generate static keys until the code on the phone matches
the code it is showing on the Mac.

A six digit code needs about one million X25519 key generations to forge. A
laptop does that in seconds. The delay looks like ordinary network latency.

This is a known class of failure. Signal uses a 60 digit safety number for the
same reason. Bluetooth numeric comparison uses six digits safely only because
each side commits to a random nonce before the nonces are revealed.

## Decision

Add a commit and reveal step inside the `XX` message payloads.

1. The initiator commits to `BLAKE3(static_public_key_a || nonce_a)` in message
   one.
2. The responder sends `nonce_b` in message two.
3. The initiator reveals `nonce_a` in message three. The responder verifies the
   commitment and aborts on a mismatch.
4. The code derives from the Noise handshake hash, `nonce_a`, and `nonce_b`.

Also require these rules:

- Pairing works only inside a user-started pairing mode on the phone, and that
  mode times out.
- Both screens show the code, and a person confirms on both.
- Pairing over the USB cable is allowed and preferred.

## Reasons

Each side is locked in before it sees the other side's input. Grinding no
longer helps, because the attacker must commit before it knows `nonce_b`. Its
chance per attempt falls to one in the size of the code space.

The pairing mode stops any device on the network from making the phone show a
code at a moment the user is not expecting one.

Confirmation on both screens stops an attacker that spoofs the phone's mDNS
name and pairs with the Mac while the real phone shows nothing.

The cable removes the network attacker completely, which is why it is
preferred.

## Consequences

The `XX` payloads now carry data, so the pairing handshake is part of the
protocol document rather than a plain Noise handshake.

The code can stay short. Six digits is enough once commitment is in place.

## Rejected alternative

A much longer code, in the style of a Signal safety number. It works, and users
do not read 60 digits carefully. Commitment gives better safety with a code
people will actually compare.

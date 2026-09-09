# 4. USB is a reliability feature, not a speed feature

Date: 9 September 2026.
Status: accepted.

## Context

The first version of this plan justified the USB path by speed. That
justification does not hold.

Android Open Accessory runs over USB 2.0 bulk endpoints. It does not negotiate
SuperSpeed. Its throughput lands in the same range as good 5 GHz Wi-Fi. Better
code cannot raise that ceiling.

## Decision

Build the USB path, and describe it as a reliability feature.

## Reasons

USB works when the network does not. Guest networks and hotel networks often
block device to device traffic, which breaks mDNS discovery and breaks every
tool that relies on it. A cable has no such failure.

A cable also has predictable behaviour. Wi-Fi throughput swings with distance,
interference, and router quality.

## Consequences

No measurement gate stands in front of this work. Measuring throughput to
decide whether to build it would change no decision, so it was cut.

Throughput numbers still go in the README once the transport works. They are a
by-product, not a gate.

The demo shows a transfer surviving a network drop, not a speed comparison. A
speed comparison would argue against the feature.

## Rejected alternative

Skipping USB and shipping only the network path. That leaves the app broken on
exactly the networks where users need it most.

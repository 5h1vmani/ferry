# 3. Noise instead of TLS

Date: 9 September 2026.
Status: accepted.

## Context

Two devices pair once with a short confirmation code. After that each device
knows the other's static public key. Every later connection must be encrypted
and must prove the peer is the same device.

The same encrypted channel has to run over a TCP socket and over a USB bulk
pipe.

## Decision

Use the Noise protocol framework. Use the `XX` pattern for first pairing, and
the `KK` pattern for every connection after that.

## Reasons

The pairing model is "pin the peer's static public key". Noise `KK` is exactly
that, with no extra machinery. There is no certificate to generate, no
self-signed certificate handling, and no public key infrastructure vocabulary
in the protocol document.

Noise runs over any byte stream. The TCP transport and the USB transport share
one implementation.

A shorter protocol document is also easier to review, and the document is part
of what this project is for.

## Consequences

No browser can connect to Ferry. That is acceptable, because no browser needs
to.

## Rejected alternative

TLS, which is what LocalSend and Syncthing use. TLS wins when a browser must
connect or when the peer set is open. Neither applies here. TLS would also add
certificate handling that carries no benefit for a pinned-key model.

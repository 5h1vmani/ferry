# How Ferry speaks

Ferry is a tool. It states. It does not persuade, reassure, apologise, or
celebrate. Every string a person reads in either app follows this file.

## The rules

1. State what is true. Never what Ferry thinks about it.
2. One fact per sentence. Most sentences are under twelve words.
3. Present tense for what is happening. Past tense for what happened.
4. Name things by their real names. "Pixel 3 XL", not "your device". "USB
   cable", not "connection". "Wi-Fi", not "network" when it is Wi-Fi.
5. Numbers instead of adjectives. "42 MB/s", never "fast". "3 of 120 files",
   never "almost done".
6. No exclamation marks. No emoji. No "oops", "great", "sorry", "please", or
   "just".
7. Never blame the person. Never blame the phone. Say what happened.
8. An error has three parts, in this order. What stopped. Why. What to do.
   Any part that is unknown is left out, not guessed.
9. A button says what it does, as a verb. "Pair", "Send", "Retry", "Forget
   this phone". Never "OK", never "Continue", never "Got it".
10. Nothing is hidden behind a friendly word. If a file failed to verify, the
    word is "failed", not "had an issue".

## Examples

Right:

    Pixel 3 XL connected over USB.
    Transfer paused. The cable was disconnected. Reconnect to continue.
    3 of 120 files copied. 2.1 GB remaining. 38 MB/s.
    Pairing code 481 920. Confirm it matches on both screens.
    Chunk 14 of IMG_0410.jpg failed to verify. The file changed on the phone during transfer. Send it again.
    Finder mount ready at /Volumes/Pixel 3 XL.
    This phone is forgotten. Pair again to reconnect.

Wrong, and why:

    Oops! Looks like we lost the connection 😕
        Apology, blame on nobody in particular, emoji, no next step.
    Almost there!
        An adjective standing in for a number.
    Something went wrong. Please try again.
        Nothing stated. No cause. "Please".
    Your device has been successfully paired!
        "Your device" instead of its name. "Successfully" adds nothing.
        Exclamation mark.
    Great! Ferry is ready to go.
        Ferry has no feelings about being ready.

## Where words come from

Strings live in one file per app and nowhere else. A string that appears
inline in a view is a bug.

The same event uses the same words on both platforms. "Paused" on the Mac is
"Paused" on the phone, never "Interrupted" on one and "Paused" on the other.

Technical terms match `docs/protocol.md` exactly. A chunk is a chunk. A session
is a session. A pairing code is a pairing code.

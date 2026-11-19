# The iPhone app

The app is Zeron's iOS app reduced to one chat (`apps/ios/Zeron/Heron/`).

## Sub-features

- Opens straight into the chat titled Heron, on whichever computer hosts it
- The ⋯ menu: Computer(s), Copy Transcript, Sign Out
- "Continued on another computer" after a takeover
- Dictation: the composer mic records and transcribes (Groq Whisper + cleanup, or on-device) into the draft, never sends. See [docs/ios-dictation.md](../../../../docs/ios-dictation.md)

## How to get to it (user POV)

Open Heron on the iPhone.

## Driving it with the simulator (on a Mac)

```
scripts/heron/sim.sh                 # build, install, launch; screenshot at /tmp/heron-sim.png
```

Proof: read the screenshot. For flows, the UI tests are the stable handle:
`apps/ios/ZeronUITests/HeronFlowTests.swift` (and `ZeronTests/HeronTests.swift`
for how the app picks the chat). GitHub Actions runs both on every push to
`main`; treat that run as the record, not as the loop.

The phone is the last step: `scripts/heron/install-on-iphone.sh` installs
CI's build. Before it, check `xcrun devicectl list devices` shows the iPhone
as available (unlocked, same Wi-Fi) and Xcode's account is signed in;
otherwise the install fails after the download.

Dictation without XCTest (the simulator runner fails to attach on the Xcode
27 beta): `scripts/ios/test-dictation-network.sh` for the client and pipeline,
then the `-dictate-on-open` launch rig in docs/ios-dictation.md. Proof is the
draft text in the screenshot and `[dictation] transcribed via merged` from
`xcrun simctl spawn booted log show --last 1m --predicate 'eventMessage CONTAINS "[dictation]"'`.

## Gotchas

- The simulator signs in to the real Zeron account and opens the person's
  real chat: a message typed there is a real message to Heron.
- The first simulator build compiles the Rust core (about 20 minutes);
  after that a Swift change is about 10 seconds.

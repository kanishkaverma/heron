# iOS dictation (Voice to Text)

The composer's mic turns speech into draft text. It never sends: the text lands
at the cursor for you to edit, and only Send sends it. This is separate from
voice calls (`Voice/VoiceStageViewController.swift` and friends).

## Use

1. Tap the mic beside Send. The composer turns into the recording bar:
   **✕ · live waveform · ■ · ↑**. ■ pauses (▶ resumes the same recording),
   ✕ discards it and restores the draft. Recordings stop at five minutes.
2. Tap **↑** to transcribe. A spinner shows while it works; ✕ still cancels.
   The text is inserted where the cursor was (or replaces the selection).
3. Touch and hold the mic for **Voice to Text Settings**: the Groq API key,
   Test Connection, and **Names and words to get right** (comma-separated;
   starts with Heron, Zeron, heronctl, nishka-server, …).

## How it transcribes

`Voice/DictationPipeline.swift`, after the recording is finished:

| Has a Groq key | What runs | Result |
| --- | --- | --- |
| yes | Groq Whisper Large v3 (language detected), then Groq `openai/gpt-oss-120b` cleans up the text: Hinglish in Roman script, your spellings | cleaned transcript |
| yes, cleanup fails or looks wrong | — | Whisper's text, or on-device if Whisper wrote Devanagari |
| yes, Groq down / offline / rate-limited | Apple's on-device SpeechTranscriber (hi-IN) | on-device transcript |
| no | on-device only (as T3 Code's iPhone app does by default) | on-device transcript |

Why this shape (measured on 17 synthetic Indian-English and Hinglish clips with
tech terms and names, word error rate, lower is better; see "Accuracy" below):

- Whisper with the language detected writes Hindi in Devanagari; forced to
  English it often translates Hindi into English, losing the words said. A
  Whisper `prompt` with Roman Hinglish fixed the script but sometimes echoed
  the prompt back instead of transcribing, so there is no prompt.
- Apple's hi-IN model writes Hinglish in Roman script and keeps English words
  English, but mishears tech terms ("heronctl" → "her until").
- A text pass that transliterates Whisper's Devanagari and applies the
  person's spellings beat both by 2.5–3×. Also feeding it the on-device
  transcript did not help (it pulled in on-device mishearings), so the
  phone's model is the fallback, not a second opinion.

The on-device model downloads once per language (needs a network that first
time). SpeechAnalyzer runs on the finished file, as in T3 Code
(`apps/mobile/src/native/voiceTranscription.ios.ts` there).

## Keys and privacy

- The Groq key is a device-only Keychain item
  (`kSecAttrAccessibleWhenUnlockedThisDeviceOnly`), entered in Voice to Text
  Settings. It is never in the repo, UserDefaults, the Rust core, sync or the
  Heron computers. Emptying the field and saving removes it.
- Audio goes from the phone straight to `api.groq.com` only when you tap ↑.
  The cleanup pass sends only Whisper's text and your word list.
- Temporary audio is deleted on success and cancel; a failed transcription
  keeps it so ↑ can retry. Nothing is retried automatically.
- Error bodies and keys are never shown or logged.

## Tests

The simulator's XCTest runner fails to attach on the Xcode 27 beta Mac
("DTServiceHubClient failed to bless service hub"); until it does, the
screenshot rig below drives the same UI without XCTest:

```sh
# Records the sample, taps ↑ after 4 s, inserts into the draft "Note: ".
SIMCTL_CHILD_ZERON_DICTATION_TEST_AUDIO=/tmp/clip.wav SIMCTL_CHILD_ZERON_DICTATION_TEST_GROQ_KEY=… \
  xcrun simctl launch "Heron iPhone" com.kanishkaverma.heron -demo -fast \
  -dictate-on-open -dictate-draft "Note: " -dictate-transcribe-after 4
# -dictate-settings opens the settings sheet instead. DEBUG builds only.
```

```sh
# Groq client + pipeline, on this Mac (no simulator), with stubbed network:
scripts/ios/test-dictation-network.sh

# Accuracy over real speech through the production pipeline:
DICTATION_LIVE_ALL=1 DICTATION_LIVE_CLIPS=/tmp/clips GROQ_API_KEY=… \
  scripts/ios/test-dictation-network.sh --filter testLiveAccuracy
```

`/tmp/clips` holds `refs.tsv` (`id<TAB>voice<TAB>reference`) and `<id>.wav`.
Make clips with `say -v Rishi -o c.aiff "…"` (Indian English voices: Rishi,
Aman, Tara; Lekha reads Devanagari Hindi with English words) and
`afconvert -f WAVE -d LEI16@16000 -c 1 c.aiff c.wav`.

In the simulator, `ZeronTests/DictationTests` adds the composer insertion and
Keychain tests, and `ZeronUITests/DictationFlowTests` drives the mic, the
recording bar, settings and (with `TEST_RUNNER_ZERON_DICTATION_TEST_GROQ_KEY`
set and a sample at `/tmp/heron-dict-heldout/h7.wav` or `DICTATION_TEST_AUDIO`)
a live transcription into an unsent draft. The simulator-only seams
`ZERON_DICTATION_TEST_AUDIO` (a file instead of the mic) and
`ZERON_DICTATION_TEST_GROQ_KEY` (a key instead of the Keychain) are compiled
out of device builds.

## Accuracy

Word error rate (case and punctuation ignored), synthetic speech from macOS
voices; 9 design clips and 8 held-out clips:

| Engine | 9 design clips | 8 held-out clips |
| --- | --- | --- |
| **Whisper Large v3 + Groq cleanup (shipped, with a key)** | **0.072** | **0.048** |
| Whisper + on-device transcripts merged by the same pass | 0.078 | 0.075 |
| Whisper alone (on-device when Whisper wrote Devanagari) | 0.177 | 0.119 |
| Apple on-device, hi-IN (shipped, without a key) | 0.224 | 0.131 |
| Apple on-device, en-IN / en-US | 0.275 / 0.274 | – |
| Whisper Large v3 raw, language detected / forced English | 0.474 / 0.345 | – |
| Whisper Large v3 Turbo raw, forced English | 0.421 | – |

The design clips include sentences like "Theek hai, Tailscale ka exit node
change karo aur Postgres backup ko S3 pe daal do" and "Open the PR on GitHub,
run cargo test for the mobile crate, then deploy with heronctl"; the held-out
clips were written after the design was fixed. Remaining errors are mostly
spelling choices ("type script", "PostGaz", "Thik" for "Theek"). Synthetic
voices are cleaner than a person in a room, so treat these as relative, not
absolute, numbers.

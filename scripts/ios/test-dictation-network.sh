#!/bin/bash
# Run the platform-neutral dictation tests (Groq client, the Whisper +
# on-device pipeline, and Apple's on-device model) on this Mac, without an iOS
# test runner. Uses the production sources and the same tests as the Xcode target.
#
# Accuracy run over real speech (opt-in; the key comes from your environment,
# never from the repo):
#   DICTATION_LIVE_CLIPS=/tmp/clips GROQ_API_KEY=… scripts/ios/test-dictation-network.sh
# where /tmp/clips holds refs.tsv (id<TAB>voice<TAB>reference text) and <id>.wav.
# DICTATION_LIVE_ALL=1 also scores on-device only and Whisper without the merge.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
DIR="$(mktemp -d "${TMPDIR:-/tmp}/zeron-dictation-tests.XXXXXX")"
trap 'rm -rf "$DIR"' EXIT
mkdir -p "$DIR/Sources/Dictation" "$DIR/Tests/DictationTests"
printf '%s\n' '// swift-tools-version: 5.9' 'import PackageDescription' 'let package = Package(name: "Dictation", platforms: [.macOS("26.0")], targets: [.target(name: "Dictation"), .testTarget(name: "DictationTests", dependencies: ["Dictation"])])' > "$DIR/Package.swift"
for f in GroqTranscription OnDeviceTranscription DictationPipeline; do
  cp "$ROOT/apps/ios/Zeron/Voice/$f.swift" "$DIR/Sources/Dictation/"
done
cp "$ROOT/apps/ios/ZeronTests/DictationTests.swift" "$DIR/Tests/DictationTests/"
cd "$DIR"
swift test "$@"

#!/usr/bin/env bash
# Heron.app for the Mac: the one Heron chat in a window.
#
# It is Zeron's zgx interface (apps/zgx) built with its `heron` feature, which
# cuts it down to the Heron chat (apps/zgx/src/heron.rs, ui/screens/heron).
# Heron.app is only a window: the account, the chats and the engine are
# Zeron.app's, which must be installed and signed in on this Mac. So unlike
# Zeron.app (scripts/package-zgx-macos.sh) the bundle has no engine inside, no
# `zeron://` scheme and no updater; it has its own bundle id, name and icon,
# and remembers its window under its own name.
#
#   scripts/heron/package-mac-app.sh             build target/heron-mac/Heron.app
#   scripts/heron/package-mac-app.sh --install   and put it in /Applications
#
#   HERON_MAC_BIN        the interface, built already (--features heron)
#   HERON_MAC_BUNDLE_ID  default com.kanishkaverma.heron.mac
#   CODESIGN_IDENTITY    default: the first Apple Development identity, else ad hoc
#
# Build it from the version of Zeron this Mac runs when you can: the window
# speaks the engine's protocol, and an engine much newer or older may not
# know a call it makes.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
command -v cargo >/dev/null 2>&1 || PATH="$HOME/.cargo/bin:$PATH"
die() { echo "$*" >&2; exit 1; }

INSTALL=0
[[ "${1:-}" == "--install" ]] && INSTALL=1
VERSION="$(grep -m1 '^version' "$ROOT/Cargo.toml" | sed 's/.*"\(.*\)".*/\1/')"
BUNDLE_ID="${HERON_MAC_BUNDLE_ID:-com.kanishkaverma.heron.mac}"
OUT_DIR="$ROOT/target/heron-mac"
APP="$OUT_DIR/Heron.app"
mkdir -p "$OUT_DIR"

BIN="${HERON_MAC_BIN:-}"
if [[ -z "$BIN" ]]; then
  # (From its folder: cargo reads .cargo/config.toml by where it runs.)
  (cd "$ROOT/apps/zgx" && ZERON_BETA_VERSION="$VERSION" cargo build --release --features heron)
  BIN="$ROOT/apps/zgx/target/release/zeron-zgx"
fi
[[ -x "$BIN" ]] || die "no interface at $BIN"
# Production's only, as Zeron.app's: nothing of staging's edge or sign-in.
if LC_ALL=C grep -aqE 'staging\.zeron\.sh|zeron-edge-staging' "$BIN"; then
  die "$BIN carries staging's edge: build it with --features heron"
fi

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
install -m 755 "$BIN" "$APP/Contents/MacOS/heron"

PLIST="$APP/Contents/Info.plist"
sed "s/__VERSION__/$VERSION/" "$ROOT/dist/macos/Info.plist" >"$PLIST"
plutil -replace CFBundleIdentifier -string "$BUNDLE_ID" "$PLIST"
plutil -replace CFBundleName -string "Heron" "$PLIST"
plutil -replace CFBundleDisplayName -string "Heron" "$PLIST"
plutil -replace CFBundleExecutable -string "heron" "$PLIST"
plutil -replace CFBundleIconFile -string "heron.icns" "$PLIST"
plutil -remove CFBundleIconName "$PLIST"
# Not Zeron.app: no engine inside, and `zeron://` links stay Zeron's.
plutil -remove ZeronEngineVersion "$PLIST"
plutil -remove CFBundleURLTypes "$PLIST"
plutil -replace LSMinimumSystemVersion -string "13.0" "$PLIST"
plutil -replace NSMicrophoneUsageDescription -string "Heron uses your microphone for on-device dictation." "$PLIST"
# (The build never updates itself; this says so to anything that asks.)
plutil -replace LSEnvironment -json '{"ZERON_AUTO_UPDATE":"0"}' "$PLIST"
plutil -convert xml1 "$PLIST"
plutil -lint "$PLIST" >/dev/null

# The scripts (read where they lie). Not zgx's generated types or the tests.
rsync -a --exclude '.zgx/' --exclude '/tests/' --exclude '.lock' --exclude '.DS_Store' \
  "$ROOT/apps/zgx/ui/" "$APP/Contents/Resources/ui/"

mkdir -p "$APP/Contents/Resources/licenses/fonts"
cp "$ROOT/crates/ui/assets/fonts/licenses/"* "$ROOT/apps/zgx/fonts/"*-OFL.txt "$APP/Contents/Resources/licenses/fonts/"
cp "$ROOT/THIRD_PARTY_NOTICES.md" "$ROOT/LICENSE" "$APP/Contents/Resources/licenses/"

# Heron's icon: the iPhone app's, as an .icns.
ICON_SRC="$ROOT/apps/ios/Zeron/Assets.xcassets/AppIcon.appiconset/AppIcon1024.png"
ICONSET="$OUT_DIR/heron.iconset"
rm -rf "$ICONSET" && mkdir -p "$ICONSET"
for size in 16 32 128 256 512; do
  sips -z "$size" "$size" "$ICON_SRC" --out "$ICONSET/icon_${size}x${size}.png" >/dev/null
  sips -z "$((size * 2))" "$((size * 2))" "$ICON_SRC" --out "$ICONSET/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/heron.icns"
rm -rf "$ICONSET"

# A real identity keeps macOS's microphone permission across rebuilds; ad hoc
# works, and asks again after each.
IDENTITY="${CODESIGN_IDENTITY:-}"
if [[ -z "$IDENTITY" ]]; then
  IDENTITY="$(security find-identity -v -p codesigning 2>/dev/null | sed -n 's/.*"\(Apple Development:[^"]*\)".*/\1/p' | head -1)"
fi
codesign --force --entitlements "$ROOT/dist/macos/Zeron.entitlements" --sign "${IDENTITY:--}" --identifier "$BUNDLE_ID" "$APP"
codesign --verify --strict "$APP"
echo "built: $APP ($(du -sh "$APP" | cut -f1))"

if [[ "$INSTALL" == "1" ]]; then
  DEST="/Applications/Heron.app"
  if pgrep -f -x "$DEST/Contents/MacOS/heron" >/dev/null 2>&1; then
    die "Heron is open: quit it, then run this again"
  fi
  # Whatever is there now goes to the Trash, not away.
  if [[ -e "$DEST" ]]; then
    mv "$DEST" "$HOME/.Trash/Heron $(date +%Y%m%d-%H%M%S).app"
  fi
  ditto "$APP" "$DEST"
  echo "installed: $DEST"
fi

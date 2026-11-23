#!/bin/bash
# Builds a best-effort macOS app bundle (unsigned, ad-hoc signed) and zips it into dist/.
#
# Needs Homebrew with: brew install mpv ffmpeg dylibbundler
# libmpv, ffmpeg and ffprobe come from Homebrew; dylibbundler copies their dylibs into
# Keepcut.app/Contents/Frameworks and rewrites the load paths.
#
# Users open it the first time with right-click → Open (or System Settings → Privacy &
# Security → Open Anyway), because it is not notarized.
set -euo pipefail

root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$root"
version=$(sed -n '/^\[workspace\.package\]/,/^\[/s/^version *= *"\(.*\)"/\1/p' Cargo.toml)
brew=$(brew --prefix)
arch=$(uname -m)

export LIBRARY_PATH="$brew/lib${LIBRARY_PATH:+:$LIBRARY_PATH}"
cargo build --release --locked -p keepcut -p keepcut-cli

app=dist/Keepcut.app
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources" "$app/Contents/Frameworks"
cp target/release/keepcut target/release/keepcut-cli "$app/Contents/MacOS/"
cp "$(brew --prefix ffmpeg)/bin/ffmpeg" "$(brew --prefix ffmpeg)/bin/ffprobe" "$app/Contents/MacOS/"
chmod u+w "$app/Contents/MacOS/"*

dylibbundler -od -b -cd \
  -x "$app/Contents/MacOS/keepcut" \
  -x "$app/Contents/MacOS/ffmpeg" \
  -x "$app/Contents/MacOS/ffprobe" \
  -d "$app/Contents/Frameworks/" \
  -p @executable_path/../Frameworks/

# Smart cuts of AV1 clips need a software AV1 encoder; Homebrew's ffmpeg has SVT-AV1.
# Captured first: with pipefail, grep -q exiting early would kill ffmpeg with SIGPIPE.
encoders=$("$app/Contents/MacOS/ffmpeg" -hide_banner -encoders)
if ! grep -q libsvtav1 <<<"$encoders"; then
  echo "the bundled ffmpeg has no libsvtav1 encoder" >&2
  exit 1
fi

# Icon
iconset=$(mktemp -d)/keepcut.iconset
mkdir -p "$iconset"
for s in 16 32 128 256; do
  cp "packaging/icons/keepcut-$s.png" "$iconset/icon_${s}x${s}.png"
done
cp packaging/icons/keepcut-32.png "$iconset/icon_16x16@2x.png"
cp packaging/icons/keepcut-64.png "$iconset/icon_32x32@2x.png"
cp packaging/icons/keepcut-256.png "$iconset/icon_128x128@2x.png"
iconutil -c icns -o "$app/Contents/Resources/keepcut.icns" "$iconset"

cat > "$app/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Keepcut</string>
  <key>CFBundleDisplayName</key><string>Keepcut</string>
  <key>CFBundleIdentifier</key><string>io.github.borisburger.Keepcut</string>
  <key>CFBundleVersion</key><string>$version</string>
  <key>CFBundleShortVersionString</key><string>$version</string>
  <key>CFBundleExecutable</key><string>keepcut</string>
  <key>CFBundleIconFile</key><string>keepcut</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>LSMinimumSystemVersion</key><string>13.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>CFBundleDocumentTypes</key>
  <array>
    <dict>
      <key>CFBundleTypeName</key><string>Keepcut project</string>
      <key>CFBundleTypeExtensions</key><array><string>keepcut</string></array>
      <key>CFBundleTypeRole</key><string>Editor</string>
    </dict>
    <dict>
      <key>CFBundleTypeName</key><string>Video</string>
      <key>LSItemContentTypes</key><array><string>public.movie</string></array>
      <key>CFBundleTypeRole</key><string>Viewer</string>
      <key>LSHandlerRank</key><string>Alternate</string>
    </dict>
  </array>
</dict>
</plist>
EOF

mkdir -p "$app/Contents/Resources/licenses"
cp LICENSE "$app/Contents/Resources/licenses/Keepcut-GPL-3.0.txt"

# Rewritten binaries must be re-signed (ad-hoc) or Apple Silicon refuses to run them.
codesign --force --deep --sign - "$app"

zip="dist/Keepcut-$version-macos-$arch.zip"
rm -f "$zip"
ditto -c -k --keepParent "$app" "$zip"
echo "zip: $zip ($(du -h "$zip" | cut -f1))"

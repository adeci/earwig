#!/usr/bin/env bash
# Wrap the release probe in an ad-hoc signed app bundle, so macOS asks for
# privacy permissions on behalf of "earwig probe" rather than the terminal.
#   probes/macos/bundle.sh <out dir>   ->  <out dir>/EarwigProbe.app
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
out=${1:?out dir}
app="$out/EarwigProbe.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS"
cp "$here/target/release/earwig-macos-probe" "$app/Contents/MacOS/earwig-macos-probe"
cat >"$app/Contents/Info.plist" <<'EOF'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleIdentifier</key><string>io.github.adeci.earwig.probe</string>
  <key>CFBundleName</key><string>earwig probe</string>
  <key>CFBundleExecutable</key><string>earwig-macos-probe</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>0.0.0</string>
  <key>CFBundleVersion</key><string>0</string>
  <key>LSMinimumSystemVersion</key><string>13.0</string>
  <key>LSUIElement</key><true/>
  <key>NSMicrophoneUsageDescription</key><string>earwig probe records a few seconds to test the microphone.</string>
</dict>
</plist>
EOF
/usr/bin/codesign --force --sign - --identifier io.github.adeci.earwig.probe "$app"
/usr/bin/codesign --verify --verbose "$app"
echo "$app"

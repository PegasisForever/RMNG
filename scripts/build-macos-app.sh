#!/usr/bin/env bash
# Build the native macOS viewer and wrap it in a .app bundle.
#
# The binary links only system frameworks (AppKit, Metal, VideoToolbox, CoreVideo, CoreMedia), so
# unlike the GTK viewer the bundle is genuinely self-contained: no Homebrew, nothing to copy in,
# and it runs on a Mac that has never seen this repo.
#
#   scripts/build-macos-app.sh [OUTPUT_DIR]
#
# OUTPUT_DIR defaults to target/macos. Install by dragging the bundle to /Applications, or:
#   cp -R "target/macos/RMNG Viewer.app" /Applications/
set -euo pipefail

out_dir="${1:-target/macos}"
app="$out_dir/RMNG Viewer.app"
bin_name="rmng-viewer-macos"
version="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
version="${version:-0.1.0}"

cd "$(dirname "$0")/.."
echo "building $bin_name (release)…"
cargo build -p viewer-macos --release

rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
printf 'APPL????' > "$app/Contents/PkgInfo"
cp "target/release/$bin_name" "$app/Contents/MacOS/$bin_name"
chmod 755 "$app/Contents/MacOS/$bin_name"

cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleName</key>
	<string>RMNG Viewer</string>
	<key>CFBundleDisplayName</key>
	<string>RMNG Viewer</string>
	<key>CFBundleIdentifier</key>
	<string>dev.rmng.viewer.macos</string>
	<key>CFBundleExecutable</key>
	<string>$bin_name</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleInfoDictionaryVersion</key>
	<string>6.0</string>
	<key>CFBundleVersion</key>
	<string>$version</string>
	<key>CFBundleShortVersionString</key>
	<string>$version</string>
	<key>LSMinimumSystemVersion</key>
	<string>11.0</string>
	<key>LSApplicationCategoryType</key>
	<string>public.app-category.utilities</string>
	<key>NSHighResolutionCapable</key>
	<true/>
</dict>
</plist>
PLIST

# Ad-hoc signature: arm64 requires a valid one, and re-signing after replacing the binary is what
# keeps a rebuilt bundle launchable.
codesign --force --sign - "$app"
codesign --verify --verbose=2 "$app"

echo
echo "built: $app"
echo "links only system frameworks:"
otool -L "$app/Contents/MacOS/$bin_name" | grep -c "/System/Library/Frameworks" | xargs printf '  %s system frameworks, '
otool -L "$app/Contents/MacOS/$bin_name" | grep -ci homebrew | xargs printf '%s Homebrew libraries\n'

#!/bin/sh
# Builds the macOS (Apple Silicon) package: an app bundle in a disk image.
#
# usage: scripts/package-macos.sh
#
# llama.cpp is linked statically with its Metal backend (the shaders are embedded), so the
# bundle needs no dynamic libraries. The bundle is signed ad hoc unless CODESIGN_IDENTITY names a
# signing identity; an ad-hoc signed app downloaded from the internet is blocked by Gatekeeper
# until it is notarized or its quarantine attribute is removed. With an identity, the bundle is
# signed with the hardened runtime and a secure timestamp, as notarization requires.
#
# Environment:
#   MACOSX_DEPLOYMENT_TARGET  oldest macOS to run on (default 13.3, as llama.cpp's Apple builds)
#   CODESIGN_IDENTITY         signing identity (default "-", ad hoc)
#   PACKAGE_VERSION           the version in the package's name (default: from the tags, below)
#
# Output: dist/<package>/ and <package>.dmg in the repository root.
set -eu
cd "$(dirname "$0")/.."

test "$(uname -s)" = Darwin || { echo "this script runs on macOS" >&2; exit 1; }

if ! cargo about --version >/dev/null 2>&1; then
    echo "cargo-about is required to generate third-party notices." >&2
    echo "Install it with: brew install cargo-about (or cargo install cargo-about --locked)" >&2
    exit 1
fi

export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-13.3}"
identity="${CODESIGN_IDENTITY:--}"

# The version, as the Release workflow names it (which passes it): X.Y.Z on a matching tag, else
# X.Y.Z-dev.<commit>.
crate=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)
if [ -n "${PACKAGE_VERSION:-}" ]; then
    version=$PACKAGE_VERSION
elif git describe --exact-match --tags --match "v$crate" HEAD >/dev/null 2>&1; then
    version=$crate
else
    version="$crate-dev.$(git rev-parse --short HEAD)"
fi
package="plamo-translate-desktop-$version-macos-arm64"
app_name="PLaMo Translate Desktop"
echo "Packaging $package (macOS $MACOSX_DEPLOYMENT_TARGET or later)"

cargo build --workspace --profile dist --bins --examples --locked --target-dir target

scripts/notices.sh macos THIRD-PARTY-NOTICES.txt

dist="dist/$package"
app="$dist/$app_name.app"
rm -rf "$dist" "$package.dmg"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"

cp target/dist/plamo-translate-desktop target/dist/plamo-translate-host "$app/Contents/MacOS/"
# The app looks for `browser-extension` next to its executable. Code goes in Contents/MacOS and
# other files in Contents/Resources, so the folder is there and linked.
cp -R extension "$app/Contents/Resources/browser-extension"
ln -s ../Resources/browser-extension "$app/Contents/MacOS/browser-extension"

# The icon, rendered from the same design as the other platforms' (scripts/gen_icons.py), with
# the margin macOS icons have: the tile covers 824 of 1024 pixels.
iconset=$(mktemp -d)/AppIcon.iconset
mkdir -p "$iconset"
python3 - "$iconset/icon_512x512@2x.png" <<'EOF'
import sys
sys.path.insert(0, "scripts")
import gen_icons

canvas, tile = 1024, 824
margin = (canvas - tile) // 2
rows = gen_icons.render(tile, ss=2)
empty = (0, 0, 0, 0)
padded = [[empty] * canvas for _ in range(margin)]
padded += [[empty] * margin + row + [empty] * margin for row in rows]
padded += [[empty] * canvas for _ in range(canvas - len(padded))]
with open(sys.argv[1], "wb") as f:
    f.write(gen_icons.png_bytes(padded))
EOF
for size in 16 32 128 256 512; do
    sips -z "$size" "$size" "$iconset/icon_512x512@2x.png" --out "$iconset/icon_${size}x${size}.png" >/dev/null
    double=$((size * 2))
    if [ "$size" != 512 ]; then
        sips -z "$double" "$double" "$iconset/icon_512x512@2x.png" --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
    fi
done
iconutil -c icns "$iconset" -o "$app/Contents/Resources/AppIcon.icns"
rm -rf "$(dirname "$iconset")"

cat > "$app/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleDevelopmentRegion</key>
    <string>en</string>
    <key>CFBundleDisplayName</key>
    <string>$app_name</string>
    <key>CFBundleExecutable</key>
    <string>plamo-translate-desktop</string>
    <key>CFBundleIconFile</key>
    <string>AppIcon</string>
    <key>CFBundleIdentifier</key>
    <string>com.github.tanakh.plamo-translate-desktop</string>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>CFBundleName</key>
    <string>PLaMo Translate</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>$crate</string>
    <key>CFBundleVersion</key>
    <string>$crate</string>
    <key>LSApplicationCategoryType</key>
    <string>public.app-category.productivity</string>
    <key>LSMinimumSystemVersion</key>
    <string>$MACOSX_DEPLOYMENT_TARGET</string>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>NSHumanReadableCopyright</key>
    <string>Copyright (c) 2026 Hideyuki Tanaka</string>
    <key>NSPrincipalClass</key>
    <string>NSApplication</string>
</dict>
</plist>
EOF
plutil -lint "$app/Contents/Info.plist"

# What the app needs to update itself; inside the bundle, so before it is signed.
python3 scripts/package-marker.py "$app/Contents/Resources" macos-arm64 --no-entries

# Nested code first, then the bundle. Notarization, which takes a Developer ID, also needs the
# hardened runtime and a secure timestamp.
if [ "$identity" = - ]; then
    sign_options="--timestamp=none"
else
    sign_options="--options runtime --timestamp"
fi
# shellcheck disable=SC2086 # sign_options holds separate arguments.
codesign --force $sign_options --sign "$identity" "$app/Contents/MacOS/plamo-translate-host"
# shellcheck disable=SC2086
codesign --force $sign_options --sign "$identity" "$app"
codesign --verify --strict --verbose=2 "$app"

cp README.md README.ja.md LICENSE-MIT LICENSE-APACHE THIRD-PARTY-NOTICES.txt "$dist/"
ln -s /Applications "$dist/Applications"
ls -l "$dist" "$app/Contents/MacOS"

# Smoke test: the CLI (a development tool, not shipped) must list at least the CPU, and the host
# in the bundle must start.
target/dist/examples/translate --list-devices
"$app/Contents/MacOS/plamo-translate-host" --check

# hdiutil fails now and then on CI machines (a busy resource): it is tried again.
attempt=1
until hdiutil create -volname "$app_name" -srcfolder "$dist" -fs HFS+ -format UDZO -ov "$package.dmg"; do
    if [ "$attempt" -ge 3 ]; then
        echo "hdiutil failed $attempt times" >&2
        exit 1
    fi
    attempt=$((attempt + 1))
    sleep 10
done
shasum -a 256 "$package.dmg"

#!/bin/bash
# Build a native macOS app bundle. Run on each architecture for separate packages.
set -euo pipefail
cd "$(dirname "$0")/.."
[[ $(uname -s) == Darwin ]] || { echo 'Run this script on macOS.' >&2; exit 1; }
if ! xcrun -sdk macosx metal --version >/dev/null 2>&1; then
    echo 'Metal compiler is required. Install full Xcode and run:' >&2
    echo '  xcodebuild -downloadComponent MetalToolchain' >&2
    exit 1
fi
export MACOSX_DEPLOYMENT_TARGET=${MACOSX_DEPLOYMENT_TARGET:-13.3}
# Embed Metal shaders so the app does not depend on files in the build tree.
export GGML_METAL_EMBED_LIBRARY=ON
cargo build --workspace --profile dist --bins --examples --locked
profile="${CARGO_TARGET_DIR:-target}/dist"
destination='dist/PLaMo Translate Desktop.app'
mkdir -p dist
staging=$(mktemp -d dist/.macos.XXXXXX)
app="$staging/PLaMo Translate Desktop.app"
cleanup() {
    # Restore the previous bundle if installing the new one failed.
    if [[ -d "$staging/previous.app" && ! -e "$destination" ]]; then
        mv "$staging/previous.app" "$destination"
    fi
    rm -rf "$staging"
}
trap cleanup EXIT
# Never overwrite a running executable: modifying its mapped pages invalidates
# macOS code signatures, even if the bundle is signed again afterwards.
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$profile/plamo-translate-desktop" "$profile/plamo-translate-host" "$app/Contents/MacOS/"
# Keep the extension next to the executable, as expected by the settings dialog.
cp -R extension "$app/Contents/MacOS/browser-extension"
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)
cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleExecutable</key><string>plamo-translate-desktop</string>
<key>CFBundleIdentifier</key><string>com.github.tanakh.plamo-translate-desktop</string>
<key>CFBundleName</key><string>PLaMo Translate Desktop</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleShortVersionString</key><string>$version</string>
<key>CFBundleVersion</key><string>$version</string>
<key>LSMinimumSystemVersion</key><string>$MACOSX_DEPLOYMENT_TARGET</string>
<key>NSHighResolutionCapable</key><true/>
</dict></plist>
PLIST
cp README.md README.ja.md LICENSE-MIT LICENSE-APACHE "$app/Contents/Resources/"
if [[ -f THIRD-PARTY-NOTICES.txt ]]; then
    cp THIRD-PARTY-NOTICES.txt "$app/Contents/Resources/"
fi
plutil -lint "$app/Contents/Info.plist"
codesign --force --deep --sign - "$app"
"$app/Contents/MacOS/plamo-translate-host" --check
"$profile/examples/translate" --list-devices
codesign --verify --deep --strict "$app"
if [[ -e "$destination" ]]; then
    mv "$destination" "$staging/previous.app"
fi
mv "$app" "$destination"
printf 'Built %s\n' "$destination"

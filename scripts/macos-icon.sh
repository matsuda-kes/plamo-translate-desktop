#!/bin/sh
# Convert the checked-in application icon to a macOS icon bundle.
# Usage: sh scripts/macos-icon.sh OUTPUT.icns
set -eu
if [ "$#" -ne 1 ]; then
    echo "Usage: $0 OUTPUT.icns" >&2
    exit 1
fi
source_icon="$(dirname "$0")/../assets/icon/icon-256.png"
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
iconset="$scratch/AppIcon.iconset"
mkdir -p "$iconset"
for size in 16 32 128 256 512; do
    sips -z "$size" "$size" "$source_icon" \
        --out "$iconset/icon_${size}x${size}.png" >/dev/null
    double=$((size * 2))
    sips -z "$double" "$double" "$source_icon" \
        --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$iconset" -o "$1"

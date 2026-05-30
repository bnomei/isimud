#!/usr/bin/env bash
set -euo pipefail

BIN_NAME="${BIN_NAME:-isimud}"
PRODUCT_NAME="${PRODUCT_NAME:-Isimud}"
BUNDLE_IDENTIFIER="${BUNDLE_IDENTIFIER:-com.bnomei.isimud}"
OUT_DIR="${OUT_DIR:-dist}"
INFO_PLIST_TEMPLATE="${INFO_PLIST_TEMPLATE:-packaging/macos/Info.plist.template}"
ICON_PATH="${ICON_PATH:-packaging/macos/Isimud.icns}"
VERSION="${VERSION:-$(scripts/resolve-version.sh)}"
CODESIGN_APP="${CODESIGN_APP:-1}"
CODESIGN_IDENTITY="${CODESIGN_IDENTITY:--}"
ZIP_APP="${ZIP_APP:-1}"

if [[ -n "${BIN_PATH:-}" ]]; then
  resolved_bin_path="$BIN_PATH"
elif [[ -n "${TARGET:-}" ]]; then
  resolved_bin_path="target/${TARGET}/release/${BIN_NAME}"
else
  resolved_bin_path="target/release/${BIN_NAME}"
fi

if [[ ! -f "$resolved_bin_path" ]]; then
  echo "Binary not found: $resolved_bin_path" >&2
  echo "Build it first with 'cargo build --release --bin $BIN_NAME' or set BIN_PATH/TARGET." >&2
  exit 1
fi

if [[ ! -f "$INFO_PLIST_TEMPLATE" ]]; then
  echo "Info.plist template not found: $INFO_PLIST_TEMPLATE" >&2
  exit 1
fi

app_dir="$OUT_DIR/${PRODUCT_NAME}.app"
contents_dir="$app_dir/Contents"
macos_dir="$contents_dir/MacOS"
resources_dir="$contents_dir/Resources"
info_plist_path="$contents_dir/Info.plist"
zip_path="$OUT_DIR/${PRODUCT_NAME}.app.zip"

rm -rf "$app_dir"
mkdir -p "$macos_dir" "$resources_dir"
rm -f "$zip_path"

PRODUCT_NAME="$PRODUCT_NAME" \
BIN_NAME="$BIN_NAME" \
BUNDLE_IDENTIFIER="$BUNDLE_IDENTIFIER" \
VERSION="$VERSION" \
python3 - "$INFO_PLIST_TEMPLATE" "$info_plist_path" <<'PY'
import os
import sys
from pathlib import Path
from xml.sax.saxutils import escape

template_path = Path(sys.argv[1])
output_path = Path(sys.argv[2])
content = template_path.read_text(encoding="utf-8")
replacements = {
    "@PRODUCT_NAME@": escape(os.environ["PRODUCT_NAME"]),
    "@EXECUTABLE_NAME@": escape(os.environ["BIN_NAME"]),
    "@BUNDLE_IDENTIFIER@": escape(os.environ["BUNDLE_IDENTIFIER"]),
    "@VERSION@": escape(os.environ["VERSION"]),
}
for key, value in replacements.items():
    content = content.replace(key, value)
output_path.write_text(content, encoding="utf-8")
PY

cp "$resolved_bin_path" "$macos_dir/$BIN_NAME"
chmod +x "$macos_dir/$BIN_NAME"

if [[ -f "$ICON_PATH" ]]; then
  cp "$ICON_PATH" "$resources_dir/${PRODUCT_NAME}.icns"
fi

if [[ "$CODESIGN_APP" != "0" ]]; then
  if ! command -v codesign >/dev/null 2>&1; then
    echo "codesign not found; install Xcode command line tools or set CODESIGN_APP=0 to skip signing" >&2
    exit 1
  fi

  codesign --force --deep --sign "$CODESIGN_IDENTITY" "$app_dir"
  codesign --verify --deep --strict "$app_dir"
fi

if [[ "$ZIP_APP" != "0" ]]; then
  if command -v ditto >/dev/null 2>&1; then
    ditto -c -k --sequesterRsrc --keepParent "$app_dir" "$zip_path"
  else
    echo "warning: ditto not found; skipping app zip archive" >&2
  fi
fi

echo "Built app bundle: $app_dir"
if [[ -f "$zip_path" ]]; then
  echo "Built app archive: $zip_path"
fi

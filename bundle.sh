#!/usr/bin/env bash
#
# Assemble omma into an Omma.app bundle. The bundle (with its Info.plist
# bundle identifier) is what lets macOS deliver native Notification Center
# banners; running the bare binary disables notifications.
#
# Usage: ./bundle.sh        (builds release, then bundles + ad-hoc signs)

set -euo pipefail
cd "$(dirname "$0")"

APP="Omma.app"
BIN="omma"

echo "==> cargo build --release"
cargo build --release

echo "==> assembling ${APP}"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp packaging/Info.plist "$APP/Contents/Info.plist"
cp "target/release/${BIN}" "$APP/Contents/MacOS/${BIN}"

# Ad-hoc signature: required for notifications to be delivered from an
# otherwise unsigned app.
echo "==> codesign (ad-hoc)"
codesign --force --deep --sign - "$APP"

cat <<EOF

Built ${PWD}/${APP}

Start it:
    open ${PWD}/${APP}
  or run the inner binary directly (also resolves the bundle id):
    ${PWD}/${APP}/Contents/MacOS/${BIN}

Optional: put it on your PATH for the CLI controls
    ln -sf "${PWD}/${APP}/Contents/MacOS/${BIN}" /usr/local/bin/omma
    omma --status
    omma --take-break
EOF

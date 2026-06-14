#!/usr/bin/env bash
#
# Buduje instalator .dmg dla parley (Apple Silicon).
#
# Produkt: dist/parley-<wersja>.dmg
#   - parley.app  — dwuklik w Finderze otwiera Terminal i uruchamia parley (TUI)
#   - skrót do /Applications do przeciągnięcia
#
# UWAGA (Gatekeeper): .app jest podpisany tylko ad-hoc, NIE notaryzowany.
# Na obcym Macu pierwsze uruchomienie: prawy klik -> Otwórz, albo
#   xattr -dr com.apple.quarantine /Applications/parley.app
#
set -euo pipefail

cd "$(dirname "$0")/.."
ROOT="$(pwd)"

# cargo nie jest w domyślnym PATH na tej maszynie (brak ~/.cargo/bin).
TOOLCHAIN_BIN="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin"
[ -d "$TOOLCHAIN_BIN" ] && export PATH="$TOOLCHAIN_BIN:$PATH"

# --- konfiguracja ---
APP_NAME="parley"
BUNDLE_ID="io.github.zbyhoo.parley"
VERSION="$(awk -F'"' '/^version[[:space:]]*=/{print $2; exit}' Cargo.toml)"
SIGN_ID="${PARLEY_SIGN_ID:--}"   # domyślnie ad-hoc; nadpisz np. "Developer ID Application: ..."

# --- 1. build release ---
echo "==> build release ($VERSION)"
cargo build --release --bin parley
BIN="target/release/parley"

# --- 2. złożenie parley.app ---
STAGE="target/dmg/stage"
APP="$STAGE/$APP_NAME.app"
rm -rf "target/dmg"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"

cp "$BIN" "$APP/Contents/Resources/parley-bin"
chmod +x "$APP/Contents/Resources/parley-bin"

# launcher: otwiera Terminal i uruchamia binarkę (bez AppleScript -> bez promptu TCC)
cat > "$APP/Contents/MacOS/$APP_NAME" <<'LAUNCHER'
#!/bin/bash
DIR="$(cd "$(dirname "$0")/../Resources" && pwd)"
open -a Terminal "$DIR/parley-bin"
LAUNCHER
chmod +x "$APP/Contents/MacOS/$APP_NAME"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key>              <string>$APP_NAME</string>
  <key>CFBundleDisplayName</key>       <string>$APP_NAME</string>
  <key>CFBundleIdentifier</key>        <string>$BUNDLE_ID</string>
  <key>CFBundleVersion</key>           <string>$VERSION</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleExecutable</key>        <string>$APP_NAME</string>
  <key>CFBundlePackageType</key>       <string>APPL</string>
  <key>LSMinimumSystemVersion</key>    <string>11.0</string>
  <key>LSApplicationCategoryType</key> <string>public.app-category.developer-tools</string>
</dict>
</plist>
PLIST

# --- 3. podpis (ad-hoc domyślnie) ---
echo "==> codesign (id: $SIGN_ID)"
codesign --force -s "$SIGN_ID" "$APP/Contents/Resources/parley-bin"
codesign --force -s "$SIGN_ID" "$APP"

# --- 4. skrót do Applications + .dmg ---
ln -s /Applications "$STAGE/Applications"

mkdir -p dist
DMG="dist/$APP_NAME-$VERSION.dmg"
rm -f "$DMG"
echo "==> hdiutil create $DMG"
hdiutil create -volname "$APP_NAME" -srcfolder "$STAGE" -ov -format UDZO "$DMG" >/dev/null

echo
echo "Gotowe: $ROOT/$DMG"
echo "Rozmiar: $(du -h "$DMG" | cut -f1)"
echo "Podpis:  $(codesign -dv "$APP" 2>&1 | awk -F= '/Authority/{print $2; exit}' || echo 'ad-hoc')"

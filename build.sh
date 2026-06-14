#!/usr/bin/env bash
#
# Buduje produkcyjną binarkę `parley` (release, Apple Silicon / aarch64).
# Wynik: dist/parley — samodzielna binarka, działa na innym Macu (Apple Silicon)
# bez instalowania Rusta.
#
set -euo pipefail

cd "$(dirname "$0")"

# cargo/rustc nie są w domyślnym PATH na tej maszynie (brak ~/.cargo/bin).
TOOLCHAIN_BIN="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin"
if [ -d "$TOOLCHAIN_BIN" ]; then
  export PATH="$TOOLCHAIN_BIN:$PATH"
fi

if ! command -v cargo >/dev/null 2>&1; then
  echo "błąd: nie znaleziono 'cargo' w PATH ani w $TOOLCHAIN_BIN" >&2
  exit 1
fi

echo "==> cargo build --release --bin parley"
cargo build --release --bin parley

mkdir -p dist
cp target/release/parley dist/parley

echo
echo "Gotowe: $(pwd)/dist/parley"
echo "Architektura: $(lipo -archs dist/parley 2>/dev/null || file -b dist/parley)"
echo "Rozmiar:      $(du -h dist/parley | cut -f1)"

#!/usr/bin/env bash
#
# Wydaje parley przez Homebrew tap (gotowa binarka, Apple Silicon).
#
# Co robi (idempotentnie, można uruchamiać dla kolejnych wersji):
#   1. build release binarki (aarch64-apple-darwin)
#   2. pakuje ją w tarball dist/parley-<wersja>-aarch64-apple-darwin.tar.gz
#   3. tworzy/aktualizuje GitHub Release w PUBLICZNYM repo tapa (asset = tarball)
#   4. generuje Formula/parley.rb (url + sha256) i pushuje do repo tapa
#
# Repo źródłowe może zostać prywatne — binarka i formuła żyją w repo tapa.
#
# Wymagania: gh (zalogowany), git, cargo (rustup toolchain), shasum.
# Instalacja u użytkownika po release:
#   brew install zbyhoo/parley/parley
#
set -euo pipefail

cd "$(dirname "$0")/.."
ROOT="$(pwd)"

# --- konfiguracja (nadpisywalna przez env) ---
SRC_REPO="${PARLEY_SRC_REPO:-zbyhoo/parley-tui}"
TAP_REPO="${PARLEY_TAP_REPO:-zbyhoo/homebrew-parley}"
TUPLE="aarch64-apple-darwin"

# cargo nie jest w domyślnym PATH (brak ~/.cargo/bin).
TOOLCHAIN_BIN="$HOME/.rustup/toolchains/stable-${TUPLE}/bin"
[ -d "$TOOLCHAIN_BIN" ] && export PATH="$TOOLCHAIN_BIN:$PATH"

VERSION="$(awk -F'"' '/^version[[:space:]]*=/{print $2; exit}' Cargo.toml)"
TAG="v$VERSION"
TARBALL_NAME="parley-${VERSION}-${TUPLE}.tar.gz"
TARBALL="dist/${TARBALL_NAME}"

echo "==> parley $VERSION  (tap: $TAP_REPO)"

# --- 1. build ---
command -v cargo >/dev/null 2>&1 || { echo "błąd: brak 'cargo' w PATH" >&2; exit 1; }
echo "==> cargo build --release --bin parley"
cargo build --release --bin parley

# --- 2. tarball (binarka w korzeniu archiwum) ---
STAGE="target/release-pkg"
rm -rf "$STAGE"; mkdir -p "$STAGE"
cp "target/release/parley" "$STAGE/parley"
chmod +x "$STAGE/parley"
mkdir -p dist
rm -f "$TARBALL"
tar -czf "$TARBALL" -C "$STAGE" parley
SHA="$(shasum -a 256 "$TARBALL" | awk '{print $1}')"
echo "==> $TARBALL"
echo "    sha256: $SHA"

# --- 3. GitHub Release w repo tapa (asset publiczny) ---
echo "==> release $TAG @ $TAP_REPO"
if gh release view "$TAG" --repo "$TAP_REPO" >/dev/null 2>&1; then
  gh release upload "$TAG" "$TARBALL" --repo "$TAP_REPO" --clobber
else
  gh release create "$TAG" "$TARBALL" \
    --repo "$TAP_REPO" \
    --title "parley $VERSION" \
    --notes "Prebuilt parley $VERSION ($TUPLE). Install: \`brew install zbyhoo/parley/parley\`"
fi

URL="https://github.com/${TAP_REPO}/releases/download/${TAG}/${TARBALL_NAME}"

# --- 4. formuła w repo tapa ---
TAP_DIR="target/tap"
if [ ! -d "$TAP_DIR/.git" ]; then
  rm -rf "$TAP_DIR"
  gh repo clone "$TAP_REPO" "$TAP_DIR"
else
  git -C "$TAP_DIR" pull --ff-only
fi

mkdir -p "$TAP_DIR/Formula"
cat > "$TAP_DIR/Formula/parley.rb" <<FORMULA
class Parley < Formula
  desc "parley — multi-agent TUI"
  homepage "https://github.com/${SRC_REPO}"
  version "${VERSION}"
  url "${URL}"
  sha256 "${SHA}"

  depends_on arch: :arm64
  depends_on :macos

  def install
    bin.install "parley"
  end

  test do
    assert_predicate bin/"parley", :executable?
  end
end
FORMULA

git -C "$TAP_DIR" add Formula/parley.rb
if git -C "$TAP_DIR" diff --cached --quiet; then
  echo "==> formuła bez zmian"
else
  git -C "$TAP_DIR" commit -m "parley $VERSION"
  git -C "$TAP_DIR" push
  echo "==> formuła zaktualizowana i wypushowana"
fi

echo
echo "Gotowe. Instalacja u użytkownika:"
echo "  brew install ${TAP_REPO%/*}/${TAP_REPO#*/homebrew-}/parley"
echo "  # czyli: brew install zbyhoo/parley/parley"

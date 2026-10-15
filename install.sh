#!/bin/sh
# okbot installer for Linux and macOS: downloads a release binary from GitHub, checks it
# against the release's SHA256SUMS and installs it.
#
#   curl -fsSL https://raw.githubusercontent.com/tidusvn05/okbot/main/install.sh | sh
#
# Options: --version vX.Y.Z (default: the latest release), --dir DIR (default: ~/.local/bin). Environment: OKBOT_VERSION,
# OKBOT_INSTALL_DIR, OKBOT_DOWNLOAD_URL (a mirror of the release downloads).
# Nothing runs as root; nothing outside DIR is written.
set -eu

REPO="tidusvn05/okbot"
VERSION="${OKBOT_VERSION:-latest}"
DIR="${OKBOT_INSTALL_DIR:-$HOME/.local/bin}"
VARIANT="okbot"

say() { printf 'okbot-install: %s\n' "$*" >&2; }
fail() { say "error: $*"; exit 1; }

while [ $# -gt 0 ]; do
  case "$1" in
    --version) [ $# -ge 2 ] || fail "--version needs a value"; VERSION="$2"; shift ;;
    --dir) [ $# -ge 2 ] || fail "--dir needs a value"; DIR="$2"; shift ;;
    -h|--help)
      echo "usage: install.sh [--version vX.Y.Z] [--dir DIR]"
      echo "  --version  a release tag (default: the latest release)"
      echo "  --dir      where to put the okbot binary (default: ~/.local/bin)"
      exit 0 ;;
    *) fail "unknown option $1 (see --help)" ;;
  esac
  shift
done

if command -v curl >/dev/null 2>&1; then
  fetch() { curl -fsSL --proto '=https,file' --tlsv1.2 "$1" -o "$2"; }
  final_url() { curl -fsSLI -o /dev/null -w '%{url_effective}' "$1"; }
elif command -v wget >/dev/null 2>&1; then
  fetch() { wget -q --https-only "$1" -O "$2"; }
  final_url() { wget -q --https-only --max-redirect=5 -S --spider "$1" 2>&1 | sed -n 's/^ *Location: *//p' | tail -n 1; }
else
  fail "curl or wget is required"
fi

case "$(uname -s)" in
  Linux) os="unknown-linux-gnu" ;;
  Darwin) os="apple-darwin" ;;
  *) fail "unsupported system $(uname -s); okbot runs on Linux and macOS" ;;
esac
case "$(uname -m)" in
  x86_64|amd64) arch="x86_64" ;;
  arm64|aarch64) arch="aarch64" ;;
  *) fail "unsupported CPU $(uname -m)" ;;
esac
target="$arch-$os"

if [ "$VERSION" = "latest" ]; then
  url="$(final_url "https://github.com/$REPO/releases/latest")" || fail "cannot reach github.com"
  VERSION="${url##*/}"
  case "$VERSION" in
    v[0-9]*) ;;
    *) fail "no release found at https://github.com/$REPO/releases; before the first release, install from source (Rust 1.89+, takes a few minutes): cargo install --locked --git https://github.com/$REPO okbot" ;;
  esac
fi
case "$VERSION" in v*) ;; *) VERSION="v$VERSION" ;; esac

base="${OKBOT_DOWNLOAD_URL:-https://github.com/$REPO/releases/download}/$VERSION"
name="$VARIANT-$VERSION-$target"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT INT TERM

say "downloading $name"
fetch "$base/$name.tar.gz" "$tmp/$name.tar.gz" || fail "no $name.tar.gz in release $VERSION"
fetch "$base/SHA256SUMS" "$tmp/SHA256SUMS" || fail "no SHA256SUMS in release $VERSION"

expected="$(awk -v f="$name.tar.gz" '$2 == f || $2 == "*" f { print $1 }' "$tmp/SHA256SUMS")"
[ -n "$expected" ] || fail "$name.tar.gz is not listed in SHA256SUMS"
if command -v sha256sum >/dev/null 2>&1; then
  actual="$(sha256sum "$tmp/$name.tar.gz" | awk '{print $1}')"
else
  actual="$(shasum -a 256 "$tmp/$name.tar.gz" | awk '{print $1}')"
fi
[ "$expected" = "$actual" ] || fail "checksum mismatch for $name.tar.gz"

tar -xzf "$tmp/$name.tar.gz" -C "$tmp"
mkdir -p "$DIR"
cp "$tmp/$name/okbot" "$DIR/okbot.tmp" && chmod 755 "$DIR/okbot.tmp" && mv "$DIR/okbot.tmp" "$DIR/okbot"
say "installed $("$DIR/okbot" --version) to $DIR/okbot (checksum verified)"

case ":$PATH:" in
  *":$DIR:"*) ;;
  *) say "add $DIR to your PATH, e.g. echo 'export PATH=\"$DIR:\$PATH\"' >> ~/.profile" ;;
esac
say "next: okbot new <name>   (or ask your coding agent: okbot help-agent)"

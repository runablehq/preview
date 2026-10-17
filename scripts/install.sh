#!/bin/sh
# Installs preview from a GitHub release into ~/.local/bin.
#
#   curl -fsSL https://github.com/runablehq/preview/releases/latest/download/install.sh | sh
#
# PREVIEW_INSTALL_DIR  where to put the binary (default ~/.local/bin)
# PREVIEW_VERSION      a version such as 0.1.0 (default: the latest release)
# PREVIEW_DOWNLOAD_URL a mirror holding the release files (default: GitHub releases)
set -eu

repo=runablehq/preview
dir=${PREVIEW_INSTALL_DIR:-$HOME/.local/bin}

case "$(uname -s)-$(uname -m)" in
  Linux-x86_64) target=x86_64-unknown-linux-musl ;;
  Linux-aarch64 | Linux-arm64) target=aarch64-unknown-linux-musl ;;
  Darwin-x86_64) target=x86_64-apple-darwin ;;
  Darwin-arm64) target=aarch64-apple-darwin ;;
  *) echo "preview: no build for $(uname -s) $(uname -m)" >&2; exit 1 ;;
esac

if [ -n "${PREVIEW_DOWNLOAD_URL:-}" ]; then
  base=${PREVIEW_DOWNLOAD_URL%/}
elif [ -n "${PREVIEW_VERSION:-}" ]; then
  base="https://github.com/$repo/releases/download/v${PREVIEW_VERSION#v}"
else
  base="https://github.com/$repo/releases/latest/download"
fi

if command -v curl >/dev/null 2>&1; then
  fetch() { curl -fsSL --retry 3 -o "$2" "$1"; }
elif command -v wget >/dev/null 2>&1; then
  fetch() { wget -q -O "$2" "$1"; }
else
  echo "preview: install needs curl or wget" >&2
  exit 1
fi

if command -v sha256sum >/dev/null 2>&1; then
  sha256() { sha256sum "$1" | cut -d' ' -f1; }
else
  sha256() { shasum -a 256 "$1" | cut -d' ' -f1; }
fi

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

file="preview-$target.tar.gz"
fetch "$base/$file" "$tmp/$file"
fetch "$base/SHA256SUMS" "$tmp/SHA256SUMS"

want=$(awk -v f="$file" '$2 == f || $2 == "*" f { print $1 }' "$tmp/SHA256SUMS")
got=$(sha256 "$tmp/$file")
if [ -z "$want" ] || [ "$want" != "$got" ]; then
  echo "preview: checksum mismatch for $file (expected ${want:-nothing}, got $got)" >&2
  exit 1
fi

tar -xzf "$tmp/$file" -C "$tmp" preview
mkdir -p "$dir"
chmod 755 "$tmp/preview"
mv "$tmp/preview" "$dir/preview"

echo "Installed $("$dir/preview" --version) to $dir/preview"
case ":$PATH:" in
  *":$dir:"*) ;;
  *) echo "Add $dir to PATH to run it as preview." ;;
esac

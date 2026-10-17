#!/bin/sh
# Fills the npm packages for one release.
#
#   scripts/pack-npm.sh <version> <tarball-dir> <out-dir>
#
# <tarball-dir> holds preview-<rust-target>.tar.gz for the four targets. <out-dir> receives
# preview/ and preview-<platform>/, each ready for npm publish, every version set to <version>.
set -eu

version=${1:?version}
tarballs=${2:?tarball dir}
out=${3:?out dir}
here=$(cd "$(dirname "$0")/.." && pwd)

case "$version" in
  *[!0-9.]* | "" ) echo "pack-npm: $version is not MAJOR.MINOR.PATCH" >&2; exit 1 ;;
esac

rm -rf "$out"
mkdir -p "$out"
cp -R "$here/npm/preview" "$out/preview"
cp "$here/README.md" "$out/preview/README.md"

for pair in \
  linux-x64:x86_64-unknown-linux-musl \
  linux-arm64:aarch64-unknown-linux-musl \
  darwin-x64:x86_64-apple-darwin \
  darwin-arm64:aarch64-apple-darwin; do
  platform=${pair%%:*}
  target=${pair#*:}
  dir="$out/preview-$platform"
  cp -R "$here/npm/preview-$platform" "$dir"
  mkdir -p "$dir/bin"
  tar -xzf "$tarballs/preview-$target.tar.gz" -C "$dir/bin" preview
  chmod 755 "$dir/bin/preview"
done

# Every package.json gets the release version, and the main package pins the platform
# packages to exactly that version.
for pkg in "$out"/*/package.json; do
  node -e '
    const fs = require("fs");
    const [file, version] = process.argv.slice(1);
    const pkg = JSON.parse(fs.readFileSync(file, "utf8"));
    pkg.version = version;
    for (const dep of Object.keys(pkg.optionalDependencies || {})) pkg.optionalDependencies[dep] = version;
    fs.writeFileSync(file, JSON.stringify(pkg, null, 2) + "\n");
  ' "$pkg" "$version"
done

echo "pack-npm: packages for $version in $out"

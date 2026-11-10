#!/bin/sh
set -eu

tag=${1:-}
case "$tag" in
  v[0-9]* ) ;;
  * ) echo "usage: install.sh v<version>" >&2; exit 2 ;;
esac
case "$tag" in
  *[!A-Za-z0-9._-]* ) echo "invalid tag: $tag" >&2; exit 2 ;;
esac

case "$(uname -s)" in
  Linux) platform=linux ;;
  Darwin) platform=macos ;;
  *) echo "unsupported platform: $(uname -s)" >&2; exit 2 ;;
esac
case "$(uname -m)" in
  x86_64|amd64) arch=x86_64 ;;
  aarch64|arm64) arch=aarch64 ;;
  *) echo "unsupported architecture: $(uname -m)" >&2; exit 2 ;;
esac

repository=${TOIMIJA_REPOSITORY:-false-systems/toimija}
install_dir=${TOIMIJA_INSTALL_DIR:-"$HOME/.local/bin"}
archive="toimija-$tag-$platform-$arch.tar.gz"
url="https://github.com/$repository/releases/download/$tag"
temporary=$(mktemp -d)
trap 'rm -rf "$temporary"' EXIT HUP INT TERM

curl -fsSLo "$temporary/$archive" "$url/$archive"
curl -fsSLo "$temporary/SHA256SUMS" "$url/SHA256SUMS"
(
  cd "$temporary"
  grep "  $archive\$" SHA256SUMS > "$archive.sha256"
  if [ "$platform" = macos ]; then
    shasum -a 256 -c "$archive.sha256"
  else
    sha256sum -c "$archive.sha256"
  fi
  tar -xzf "$archive"
)
install -d "$install_dir"
install -m 0755 "$temporary/toimija" "$install_dir/toimija"
echo "installed toimija $tag to $install_dir/toimija"

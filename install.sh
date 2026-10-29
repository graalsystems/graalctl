#!/bin/sh
# Installs graalctl from the GitHub releases.
#
#   curl -fsSL https://raw.githubusercontent.com/graalsystems/graalctl/main/install.sh | sh
#
# GRAALCTL_VERSION      release tag to install (default: latest)
# GRAALCTL_INSTALL_DIR  target directory (default: /usr/local/bin, or ~/.local/bin)
set -eu

repo="graalsystems/graalctl"
version="${GRAALCTL_VERSION:-latest}"

case "$(uname -s)" in
  Linux) os=linux ;;
  Darwin) os=macos ;;
  *) echo "graalctl: unsupported OS $(uname -s); download a binary from https://github.com/$repo/releases" >&2; exit 1 ;;
esac
case "$(uname -m)" in
  x86_64 | amd64) arch=x86_64 ;;
  arm64 | aarch64) arch=aarch64 ;;
  *) echo "graalctl: unsupported architecture $(uname -m)" >&2; exit 1 ;;
esac

asset="graalctl-$os-$arch"
if [ "$version" = latest ]; then
  base="https://github.com/$repo/releases/latest/download"
else
  base="https://github.com/$repo/releases/download/$version"
fi

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
echo "Downloading $asset ($version)..."
curl -fsSL -o "$tmp/$asset" "$base/$asset"
curl -fsSL -o "$tmp/$asset.sha256" "$base/$asset.sha256"

expected="$(cut -d' ' -f1 < "$tmp/$asset.sha256")"
if command -v sha256sum >/dev/null 2>&1; then
  actual="$(sha256sum "$tmp/$asset" | cut -d' ' -f1)"
else
  actual="$(shasum -a 256 "$tmp/$asset" | cut -d' ' -f1)"
fi
if [ "$expected" != "$actual" ]; then
  echo "graalctl: checksum mismatch for $asset" >&2
  exit 1
fi

dir="${GRAALCTL_INSTALL_DIR:-/usr/local/bin}"
if [ -z "${GRAALCTL_INSTALL_DIR:-}" ] && [ ! -w "$dir" ]; then
  dir="$HOME/.local/bin"
fi
mkdir -p "$dir"
install -m 0755 "$tmp/$asset" "$dir/graalctl"
echo "Installed graalctl in $dir/graalctl"
"$dir/graalctl" version
case ":$PATH:" in
  *":$dir:"*) ;;
  *) echo "Add $dir to your PATH to use graalctl." ;;
esac

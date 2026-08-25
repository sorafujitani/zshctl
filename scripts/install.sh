#!/bin/sh
set -eu

version=${1:-${ZSHCTL_VERSION:-}}
if [ -z "$version" ]; then
  echo "usage: scripts/install.sh VERSION (for example v1.0.0)" >&2
  exit 2
fi
prefix=${ZSHCTL_INSTALL_PREFIX:-"$HOME/.local"}
case "$prefix" in
  ""|/) echo "refusing unsafe install prefix: $prefix" >&2; exit 2 ;;
esac
case "$(uname -s)-$(uname -m)" in
  Linux-x86_64) target=x86_64-unknown-linux-gnu ;;
  Linux-aarch64|Linux-arm64) target=aarch64-unknown-linux-gnu ;;
  Darwin-x86_64) target=x86_64-apple-darwin ;;
  Darwin-arm64) target=aarch64-apple-darwin ;;
  *) echo "unsupported platform: $(uname -s) $(uname -m)" >&2; exit 2 ;;
esac
archive="zshctl-$version-$target.tar.gz"
base="https://github.com/sorafujitani/zshctl/releases/download/$version"
temporary=$(mktemp -d)
trap 'rm -rf "$temporary"' EXIT HUP INT TERM
curl --fail --location --silent --show-error "$base/$archive" --output "$temporary/$archive"
curl --fail --location --silent --show-error "$base/$archive.sha256" --output "$temporary/$archive.sha256"
(cd "$temporary" && shasum -a 256 -c "$archive.sha256")
mkdir -p "$prefix/lib/zshctl/releases" "$prefix/bin"
release="$prefix/lib/zshctl/releases/$version"
if [ ! -d "$release" ]; then
  staging="$prefix/lib/zshctl/releases/.staging-$version-$$"
  mkdir "$staging"
  tar -xzf "$temporary/$archive" -C "$staging"
  test -x "$staging/zshctl" && test -x "$staging/zshctld" && test -f "$staging/zshctl.zsh" || {
    echo "release archive is missing required files" >&2
    exit 1
  }
  mv "$staging" "$release"
fi
ZSHCTL_INSTALL_PREFIX="$prefix" "$release/scripts/activate-version.sh" "$version"
echo "installed zshctl $version under $prefix"
echo "source $prefix/lib/zshctl/zshctl.zsh from your .zshrc"

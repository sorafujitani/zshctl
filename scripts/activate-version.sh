#!/bin/sh
set -eu

prefix=${ZSHCTL_INSTALL_PREFIX:-"$HOME/.local"}
version=${1:?usage: scripts/activate-version.sh VERSION}
release="$prefix/lib/zshctl/releases/$version"
test -x "$release/zshctl" || {
  echo "zshctl release is not installed: $version" >&2
  exit 2
}

mkdir -p "$prefix/bin" "$prefix/lib/zshctl"
temporary_link="$prefix/lib/zshctl/.current-$$"
ln -s "releases/$version" "$temporary_link"
if mv --version >/dev/null 2>&1; then
  mv -Tf "$temporary_link" "$prefix/lib/zshctl/current"
else
  mv -fh "$temporary_link" "$prefix/lib/zshctl/current"
fi

for name in zshctl zshctld; do
  link="$prefix/bin/.$name-$$"
  ln -s "$prefix/lib/zshctl/current/$name" "$link"
  mv -f "$link" "$prefix/bin/$name"
done
for name in zshctl.zsh shells docs spec; do
  link="$prefix/lib/zshctl/.$name-$$"
  ln -s "current/$name" "$link"
  rm -f "$prefix/lib/zshctl/$name"
  mv "$link" "$prefix/lib/zshctl/$name"
done

printf '%s\n' "$version" > "$prefix/lib/zshctl/ACTIVE_VERSION"

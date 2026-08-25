#!/bin/sh
set -eu

prefix=${ZSHCTL_INSTALL_PREFIX:-"$HOME/.local"}
case "$prefix" in
  ""|/) echo "refusing unsafe uninstall prefix: $prefix" >&2; exit 2 ;;
esac
if [ -x "$prefix/bin/zshctl" ]; then
  "$prefix/bin/zshctl" server stop >/dev/null 2>&1 || true
fi
rm -f "$prefix/bin/zshctl" "$prefix/bin/zshctld"
rm -rf "$prefix/lib/zshctl"
echo "zshctl executables removed. User configuration and history were preserved."

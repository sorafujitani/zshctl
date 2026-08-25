#!/bin/sh
set -eu

binary=${ZSHCTL_BIN:?ZSHCTL_BIN must point to a produced release binary}
root=$(mktemp -d)
home="$root/home"
prefix="$root/prefix"
runtime="$root/runtime"
mkdir -p "$home" "$prefix/bin" "$prefix/lib/zshctl/releases/v1" "$runtime"
chmod 700 "$home" "$runtime"
trap 'HOME="$home" ZSHCTL_RUNTIME_DIR="$runtime" "$prefix/bin/zshctl" server stop >/dev/null 2>&1 || true' EXIT HUP INT TERM

# Clean install from the produced artifact payload.
cp "$binary" "$prefix/lib/zshctl/releases/v1/zshctl"
cp "$binary" "$prefix/lib/zshctl/releases/v1/zshctld"
cp zshctl.zsh "$prefix/lib/zshctl/releases/v1/zshctl.zsh"
cp -R shells docs spec scripts "$prefix/lib/zshctl/releases/v1/"
ZSHCTL_INSTALL_PREFIX="$prefix" scripts/activate-version.sh v1
HOME="$home" ZSHCTL_RUNTIME_DIR="$runtime" "$prefix/bin/zshctl" server start >/dev/null
HOME="$home" ZSHCTL_RUNTIME_DIR="$runtime" "$prefix/bin/zshctl" server status >/dev/null

# An interrupted extraction does not change the active symlink.
mkdir "$prefix/lib/zshctl/releases/.staging-v2-interrupted"
printf partial > "$prefix/lib/zshctl/releases/.staging-v2-interrupted/zshctl"
test "$(readlink "$prefix/lib/zshctl/current")" = releases/v1

# Upgrade, daemon replacement, and rollback preserve history.
cp -R "$prefix/lib/zshctl/releases/v1" "$prefix/lib/zshctl/releases/v2"
ZSHCTL_INSTALL_PREFIX="$prefix" scripts/activate-version.sh v2
test "$(readlink "$prefix/lib/zshctl/current")" = releases/v2
HOME="$home" ZSHCTL_RUNTIME_DIR="$runtime" "$prefix/bin/zshctl" server restart >/dev/null
HOME="$home" ZSHCTL_RUNTIME_DIR="$runtime" "$prefix/bin/zshctl" history log "release-matrix" >/dev/null
HOME="$home" ZSHCTL_RUNTIME_DIR="$runtime" "$prefix/bin/zshctl" server stop
test ! -e "$runtime/daemon.sock"
HOME="$home" ZSHCTL_INSTALL_PREFIX="$prefix" scripts/rollback.sh v1 >/dev/null
test "$(readlink "$prefix/lib/zshctl/current")" = releases/v1
HOME="$home" ZSHCTL_RUNTIME_DIR="$runtime" "$prefix/bin/zshctl" server start >/dev/null
HOME="$home" ZSHCTL_RUNTIME_DIR="$runtime" "$prefix/bin/zshctl" history query --commands | grep -q release-matrix

# Uninstall keeps history but leaves no process or socket.
HOME="$home" ZSHCTL_RUNTIME_DIR="$runtime" ZSHCTL_INSTALL_PREFIX="$prefix" scripts/uninstall.sh >/dev/null
test ! -e "$runtime/daemon.sock"
test ! -e "$prefix/lib/zshctl"
test -f "$home/.local/share/zshctl/history.sqlite3"

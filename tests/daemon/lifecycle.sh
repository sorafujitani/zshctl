#!/bin/sh
set -eu

binary=${ZSHCTL_BIN:?ZSHCTL_BIN must point to zshctl}
runtime=$(mktemp -d)
chmod 700 "$runtime"
cleanup() {
  ZSHCTL_RUNTIME_DIR="$runtime" "$binary" server stop >/dev/null 2>&1 || true
}
trap cleanup EXIT HUP INT TERM

start_many() {
  count=0
  while [ "$count" -lt 50 ]; do
    ZSHCTL_RUNTIME_DIR="$runtime" "$binary" server start >/dev/null &
    count=$((count + 1))
  done
  wait
}

start_many
status=$(ZSHCTL_RUNTIME_DIR="$runtime" "$binary" server status)
pid=$(printf '%s' "$status" | jq -r '.health.pid')
test "$(printf '%s' "$status" | jq -r '.state')" = healthy
kill -0 "$pid"
test -S "$runtime/daemon.sock"

# Independent and nested Zsh processes receive distinct session IDs but share
# the same daemon; exiting either shell does not own daemon cleanup.
repo=$(cd "$(dirname "$0")/../.." && pwd)
shell_one=$(ZSHCTL_RUNTIME_DIR="$runtime" ZSHCTL_BIN="$binary" \
  zsh -dfc 'PATH="${ZSHCTL_BIN:h}:$PATH"; source '"$repo"'/zshctl.zsh; zshctl-init; print -r -- "$ZSHCTL_SESSION_ID"')
shell_two=$(ZSHCTL_RUNTIME_DIR="$runtime" ZSHCTL_BIN="$binary" \
  zsh -dfc 'PATH="${ZSHCTL_BIN:h}:$PATH"; source '"$repo"'/zshctl.zsh; zshctl-init; print -r -- "$ZSHCTL_SESSION_ID"')
test -n "$shell_one"
test -n "$shell_two"
test "$shell_one" != "$shell_two"
test "$(ZSHCTL_RUNTIME_DIR="$runtime" "$binary" server status | jq -r '.health.pid')" = "$pid"

if stat -f '%Lp' "$runtime" >/dev/null 2>&1; then
  test "$(stat -f '%Lp' "$runtime")" = 700
  test "$(stat -f '%Lp' "$runtime/daemon.sock")" = 600
else
  test "$(stat -c '%a' "$runtime")" = 700
  test "$(stat -c '%a' "$runtime/daemon.sock")" = 600
fi

# Simulate a crash that leaves the socket pathname behind, then repeat the race.
kill -9 "$pid"
attempt=0
while kill -0 "$pid" 2>/dev/null && [ "$attempt" -lt 100 ]; do
  attempt=$((attempt + 1))
done
start_many
recovered=$(ZSHCTL_RUNTIME_DIR="$runtime" "$binary" server status)
recovered_pid=$(printf '%s' "$recovered" | jq -r '.health.pid')
test "$(printf '%s' "$recovered" | jq -r '.state')" = healthy
test "$recovered_pid" != "$pid"
kill -0 "$recovered_pid"

# Signal-driven shutdown removes only the socket owned by this process.
kill -TERM "$recovered_pid"
attempt=0
while [ -e "$runtime/daemon.sock" ] && [ "$attempt" -lt 200 ]; do
  sleep 0.01
  attempt=$((attempt + 1))
done
test ! -e "$runtime/daemon.sock"
test ! -e "$runtime/daemon.pid"

ZSHCTL_RUNTIME_DIR="$runtime" "$binary" server start >/dev/null
ZSHCTL_RUNTIME_DIR="$runtime" "$binary" server stop >/dev/null
test ! -e "$runtime/daemon.sock"

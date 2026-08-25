#!/bin/sh
set -eu

binary=${ZSHCTL_BIN:?ZSHCTL_BIN must point to zshctl}
runtime=$(mktemp -d)
chmod 700 "$runtime"
output=$(ZSHCTL_DISABLE_DAEMON=1 ZSHCTL_RUNTIME_DIR="$runtime" "$binary" \
  --mode=preprompt --input.template='echo {{VALUE}}')
test "$(printf '%s\n' "$output" | sed -n '1p')" = success
test ! -e "$runtime/daemon.sock"

# Explicit zshctl settings are applied on a per-request basis.
output=$(ZSHCTL_DISABLE_DAEMON=1 \
  ZSHCTL_RUNTIME_DIR="$runtime" "$binary" \
  --mode=preprompt --input.template='native {{VALUE}}')
printf '%s\n' "$output" | grep -q '^native  $'
test ! -e "$runtime/daemon.sock"

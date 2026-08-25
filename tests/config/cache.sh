#!/bin/sh
set -eu

binary=${ZSHCTL_BIN:?ZSHCTL_BIN must point to zshctl}
root=$(mktemp -d)
runtime="$root/runtime"
one="$root/one"
two="$root/two"
bad="$root/bad"
mkdir "$runtime" "$one" "$two" "$bad"
chmod 700 "$runtime"
trap 'ZSHCTL_RUNTIME_DIR="$runtime" "$binary" server stop >/dev/null 2>&1 || true' EXIT HUP INT TERM

write_snippet() {
  directory=$1 text=$2
  printf 'snippets:\n  - keyword: swap\n    snippet: "%s"\n' "$text" > "$directory/config.yml"
}
write_snippet "$one" one
write_snippet "$two" two
printf 'snippets: [' > "$bad/config.yml"

request() {
  ZSHCTL_HOME="$1" ZSHCTL_RUNTIME_DIR="$runtime" "$binary" \
    --mode=auto-snippet --input.lbuffer=swap
}

request "$one" | grep -q '^one $'
daemon_pid=$(ZSHCTL_RUNTIME_DIR="$runtime" "$binary" server status | jq -r '.health.pid')
request "$two" | grep -q '^two $'
test "$(ZSHCTL_RUNTIME_DIR="$runtime" "$binary" server status | jq -r '.health.pid')" = "$daemon_pid"

# A same-shape edit invalidates the content-hash key without a restart.
write_snippet "$two" six
request "$two" | grep -q '^six $'

# A malformed context fails with its source path; another shell context remains healthy.
stderr=$(mktemp)
test "$(request "$bad" 2>"$stderr" | sed -n '1p')" = failure
grep -q "$bad/config.yml" "$stderr"
request "$one" | grep -q '^one $'

rm "$two/config.yml"
test "$(request "$two" | sed -n '1p')" = failure
if pgrep -P "$daemon_pid" >/dev/null 2>&1; then
  echo 'configuration request unexpectedly started a child process' >&2
  exit 1
fi

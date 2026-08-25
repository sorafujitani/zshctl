#!/bin/sh
set -eu

binary=${ZSHCTL_BIN:?ZSHCTL_BIN must point to zshctl}
root=$(mktemp -d)
runtime="$root/runtime"
data="$root/data"
mkdir "$runtime" "$data"
chmod 700 "$runtime" "$data"
cleanup() {
  ZSHCTL_RUNTIME_DIR="$runtime" ZSHCTL_DATA_DIR="$data" "$binary" server stop >/dev/null 2>&1 || true
}
trap cleanup EXIT HUP INT TERM

count=0
while [ "$count" -lt 100 ]; do
  ZSHCTL_RUNTIME_DIR="$runtime" ZSHCTL_DATA_DIR="$data" \
    "$binary" history log "concurrent-$count" --cwd "/tmp/session-$count" >/dev/null &
  count=$((count + 1))
done
wait

entries=$(ZSHCTL_RUNTIME_DIR="$runtime" ZSHCTL_DATA_DIR="$data" \
  "$binary" history query --limit 200 --commands | wc -l | tr -d ' ')
test "$entries" -eq 100
integrity=$(ZSHCTL_RUNTIME_DIR="$runtime" ZSHCTL_DATA_DIR="$data" \
  "$binary" history integrity | jq -r '.result')
test "$integrity" = ok

# Redaction is applied before persistence, so secrets cannot appear in DB-backed outputs.
mkdir "$root/config"
printf '%s\n' 'history:' '  redact:' "    - 'secret-[0-9]+'" > "$root/config/redact.yml"
cleanup
ZSHCTL_RUNTIME_DIR="$runtime" ZSHCTL_DATA_DIR="$data" ZSHCTL_HOME="$root/config" \
  "$binary" history log 'token secret-1234' >/dev/null
if ZSHCTL_RUNTIME_DIR="$runtime" ZSHCTL_DATA_DIR="$data" \
  "$binary" history export | grep -q 'secret-1234'; then
  echo 'redacted command leaked into export' >&2
  exit 1
fi

# UUID deletion affects one row, remains queryable with --deleted, and --hard
# prunes only the matching command from the shell's HISTFILE.
histfile="$root/zsh_history"
printf '%s\n' ': 1:0;keep-command' ': 2:0;remove-command' > "$histfile"
HISTFILE="$histfile" ZSHCTL_RUNTIME_DIR="$runtime" ZSHCTL_DATA_DIR="$data" \
  "$binary" history log --cmd remove-command --id remove-id --shell zsh >/dev/null
HISTFILE="$histfile" ZSHCTL_RUNTIME_DIR="$runtime" ZSHCTL_DATA_DIR="$data" \
  "$binary" history delete --id remove-id --hard >/dev/null
grep -q keep-command "$histfile"
if grep -q remove-command "$histfile"; then
  echo 'hard deletion did not prune the matching HISTFILE entry' >&2
  exit 1
fi
ZSHCTL_RUNTIME_DIR="$runtime" ZSHCTL_DATA_DIR="$data" \
  "$binary" history query --id remove-id --deleted only --commands | grep -q remove-command

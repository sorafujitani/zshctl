#!/bin/sh
set -eu
state_file=${1:?state file required}
client=${2:-zshctl}
cwd=${3:-$PWD}
limit=${4:-2000}
session=${5:-}
delimiter=$(printf '\302\240')
scope=$(cat "$state_file" 2>/dev/null || true)
case "${scope:-global}" in global|repository|directory|session) ;; *) scope=global ;; esac
dim=$(printf '\033[2m') reset=$(printf '\033[0m') active=$(printf '\033[1;36m')
header=
for name in global repository directory session; do
  if [ "$name" = "$scope" ]; then header="$header ${active}[${name}]${reset}"
  else header="$header ${dim}${name}${reset}"; fi
done
printf '%s\n' "${delimiter}${header# }${delimiter}${delimiter}${delimiter}${delimiter}${delimiter}"
tmp=$(mktemp "${TMPDIR:-/tmp}/zshctl-history-scope.XXXXXX")
trap 'rm -f -- "$tmp"' EXIT INT TERM
if [ -n "$session" ]; then
  "$client" history query --format smart-lines --scope "$scope" --cwd "$cwd" --limit "$limit" --session "$session" >"$tmp" 2>/dev/null || true
else
  "$client" history query --format smart-lines --scope "$scope" --cwd "$cwd" --limit "$limit" >"$tmp" 2>/dev/null || true
fi
seen=0 printed=0
while IFS= read -r line; do
  [ "$line" = success ] && { seen=1; continue; }
  [ "$seen" -eq 1 ] && [ -n "$line" ] || continue
  printed=1
  printf '%s\n' "$line"
done <"$tmp"
if [ "$printed" -eq 0 ]; then
  printf '%s\n' "__empty__${delimiter}--${delimiter}--${delimiter}${dim}(no entries)${reset}${delimiter}${delimiter}${delimiter}"
fi

#!/bin/sh
set -eu
state_file=${1:?state file required}
scope=$(cat "$state_file" 2>/dev/null || true)
case "${scope:-global}" in
  global) next=repository ;;
  repository) next=directory ;;
  directory) next=session ;;
  *) next=global ;;
esac
printf '%s\n' "$next" > "$state_file"

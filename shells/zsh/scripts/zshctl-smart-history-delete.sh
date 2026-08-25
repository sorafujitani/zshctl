#!/bin/sh
set -eu
client=${1:-zshctl}
id=${2:-}
[ -n "$id" ] && [ "$id" != __empty__ ] || exit 0
"$client" history delete --id "$id" >/dev/null 2>&1 || true

#!/bin/sh
set -eu

binary=${ZSHCTL_BIN:-"$PWD/target/release/zshctl"}
output=${ZSHCTL_BENCHMARK_OUTPUT:-spec/benchmark-results}
test -x "$binary" || { echo "build a release binary or set ZSHCTL_BIN" >&2; exit 1; }
command -v hyperfine >/dev/null 2>&1 || { echo "hyperfine is required" >&2; exit 1; }
command -v jq >/dev/null 2>&1 || { echo "jq is required" >&2; exit 1; }
command -v sqlite3 >/dev/null 2>&1 || { echo "sqlite3 is required" >&2; exit 1; }

runtime=$(mktemp -d)
data=$(mktemp -d)
home=$(mktemp -d)
chmod 700 "$runtime" "$data" "$home"
mkdir -p "$output"
cleanup() { ZSHCTL_RUNTIME_DIR="$runtime" ZSHCTL_DATA_DIR="$data" "$binary" server stop >/dev/null 2>&1 || true; }
trap cleanup EXIT HUP INT TERM

hyperfine --warmup 3 --runs 20 --export-json "$output/shell-startup.json" \
  "PATH=$(dirname "$binary"):\$PATH zsh -dfc 'source $PWD/zshctl.zsh'"
hyperfine --runs 20 \
  --prepare "ZSHCTL_RUNTIME_DIR=$runtime ZSHCTL_DATA_DIR=$data $binary server stop >/dev/null 2>&1 || true" \
  --export-json "$output/cold-request.json" \
  "ZSHCTL_RUNTIME_DIR=$runtime ZSHCTL_DATA_DIR=$data $binary server start >/dev/null"
cleanup
ZSHCTL_RUNTIME_DIR="$runtime" ZSHCTL_DATA_DIR="$data" "$binary" server start >/dev/null
hyperfine --warmup 5 --runs 50 --export-json "$output/warm-request.json" \
  "ZSHCTL_RUNTIME_DIR=$runtime $binary server status >/dev/null"

daemon_pid=$(ZSHCTL_RUNTIME_DIR="$runtime" "$binary" server status | jq -r '.health.pid')
daemon_rss_kib=$(ps -o rss= -p "$daemon_pid" | awk '{print $1}')
cleanup
ZSHCTL_RUNTIME_DIR="$runtime" ZSHCTL_DATA_DIR="$data" "$binary" history integrity >/dev/null
database="$data/history.sqlite3"
sqlite3 "$database" <<'SQL'
WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM n WHERE x < 100000)
INSERT INTO history(id,ts,command,exit,pwd,session,shell)
SELECT printf('benchmark-%06d', x), printf('%020d', x), 'benchmark-' || x,
       0, '/tmp', 'bench', 'zsh' FROM n;
SQL
ZSHCTL_RUNTIME_DIR="$runtime" ZSHCTL_DATA_DIR="$data" "$binary" server start >/dev/null
hyperfine --warmup 3 --runs 20 --export-json "$output/large-history-query.json" \
  "ZSHCTL_RUNTIME_DIR=$runtime ZSHCTL_DATA_DIR=$data $binary history query --limit 1000 >/dev/null"

metric_ms() {
  jq -r '.results[0] | if .median > 0 then .median * 1000 else (.user + .system) * 1000 end' "$1"
}
shell_ms=$(metric_ms "$output/shell-startup.json")
cold_ms=$(metric_ms "$output/cold-request.json")
warm_ms=$(metric_ms "$output/warm-request.json")
history_ms=$(metric_ms "$output/large-history-query.json")
daemon_mib=$(awk "BEGIN { print $daemon_rss_kib / 1024 }")

jq -n \
  --arg capturedAt "$(date -u '+%Y-%m-%dT%H:%M:%SZ')" \
  --arg hardware "$(uname -a)" --arg rust "$(rustc --version)" \
  --argjson shell "$shell_ms" --argjson cold "$cold_ms" --argjson warm "$warm_ms" \
  --argjson daemon "$daemon_mib" \
  --argjson history "$history_ms" \
  '{capturedAt:$capturedAt,hardware:$hardware,rust:$rust,dataset:{historyRows:100000,queryLimit:1000},results:{shellStartupP50Ms:$shell,coldRequestP50Ms:$cold,warmRequestP50Ms:$warm,daemonIdleRssMiB:$daemon,largeHistoryQueryP50Ms:$history}}' \
  > "$output/summary.json"

budgets=spec/performance-budgets.json
check_budget() {
  actual=$1 budget=$2 label=$3
  awk -v actual="$actual" -v budget="$budget" 'BEGIN { exit !(actual <= budget) }' || {
    echo "$label exceeded budget: $actual > $budget" >&2
    exit 1
  }
}
check_budget "$shell_ms" "$(jq -r '.budgets.shell_startup_p50_ms' "$budgets")" shell-startup
check_budget "$cold_ms" "$(jq -r '.budgets.cold_request_p50_ms' "$budgets")" cold-request
check_budget "$warm_ms" "$(jq -r '.budgets.warm_request_p50_ms' "$budgets")" warm-request
check_budget "$daemon_mib" "$(jq -r '.budgets.daemon_idle_rss_mib' "$budgets")" daemon-memory
check_budget "$history_ms" "$(jq -r '.budgets.large_history_query_p50_ms' "$budgets")" large-history-query
cat "$output/summary.json"

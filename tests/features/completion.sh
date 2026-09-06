#!/bin/sh
set -eu

binary=${ZSHCTL_BIN:?ZSHCTL_BIN must point to zshctl}
command -v git >/dev/null 2>&1 || exit 0
command -v fzf >/dev/null 2>&1 || exit 0
command -v ghq >/dev/null 2>&1 || exit 0
root=$(mktemp -d)
runtime="$root/runtime"
mkdir "$runtime"
chmod 700 "$runtime"
trap 'ZSHCTL_RUNTIME_DIR="$runtime" "$binary" server stop >/dev/null 2>&1 || true' EXIT HUP INT TERM

repo="$root/repository"
git init -q "$repo"
git -C "$repo" config user.name zshctl
git -C "$repo" config user.email zshctl@example.invalid
printf 'tracked\n' > "$repo/tracked.txt"
git -C "$repo" add tracked.txt
git -C "$repo" commit -qm initial
git -C "$repo" branch feature
git -C "$repo" -c tag.gpgSign=false tag v1
git -C "$repo" remote add origin https://example.invalid/repository.git
printf 'changed\n' >> "$repo/tracked.txt"

completion=$(cd "$repo" && ZSHCTL_RUNTIME_DIR="$runtime" "$binary" \
  --mode=completion --input.lbuffer='git add ')
test "$(printf '%s\n' "$completion" | sed -n '1p')" = success
source_command=$(printf '%s\n' "$completion" | sed -n '2p')
candidates=$(cd "$repo" && sh -c "$source_command")
printf '%s\n' "$candidates" | grep -q tracked.txt
printf '%s\n' "$candidates" | fzf --filter=tracked --select-1 --exit-0 | grep -q tracked.txt

snippet_config="$root/snippets.yml"
cat >"$snippet_config" <<'EOF'
snippets:
  - name: git status
    keyword: gs
    snippet: git status
EOF
snippet_list=$(ZSHCTL_DISABLE_DAEMON=1 ZSHCTL_CONFIG="$snippet_config" \
  ZSHCTL_RUNTIME_DIR="$runtime" "$binary" --mode=snippet-list)
printf '%s\n' "$snippet_list" | grep -q 'git status:.*git status.*\[gs\]'

# Exercise the real CLI protocol and fzf options together, not just mocked rows.
ZSHCTL_DISABLE_DAEMON=1 ZSHCTL_CONFIG="$snippet_config" \
  ZSHCTL_RUNTIME_DIR="$runtime" ZSHCTL_FZF_COMMAND='fzf --filter=gs' \
  zsh -dfc '
    unset ZSHCTL_BOOTSTRAPPED ZSHCTL_ROOT
    PATH="${ZSHCTL_BIN:h}:$PATH"
    source "$1/zshctl.zsh"
    zle() { :; }
    BUFFER=gs; LBUFFER=gs; RBUFFER=; CURSOR=2
    zshctl-completion
    [[ $BUFFER == "git status " && $CURSOR == 11 ]]
  ' zsh "$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)"

ghq_root="$root/ghq"
ghq_repo="$ghq_root/github.com/example/project"
mkdir -p "$ghq_repo"
git init -q "$ghq_repo"
listed=$(GHQ_ROOT="$ghq_root" ZSHCTL_RUNTIME_DIR="$runtime" "$binary" --mode=ghq-list)
printf '%s\n' "$listed" | grep -q "$ghq_repo"
printf '%s\n' "$listed" | fzf --filter=project --select-1 --exit-0 | grep -q "$ghq_repo"

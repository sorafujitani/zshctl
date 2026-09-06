#!/usr/bin/env zsh
set -eu

binary=${ZSHCTL_BIN:?ZSHCTL_BIN must point to the zshctl executable}
stdout_file=$(mktemp)
stderr_file=$(mktemp)
trap 'rm -f "$stdout_file" "$stderr_file"' EXIT

"$binary" --help >"$stdout_file" 2>"$stderr_file"
[[ -s "$stdout_file" ]]
[[ ! -s "$stderr_file" ]]

PATH="${binary:h}:$PATH"
dummy-fzf-tab-widget() { :; }
zle -N fzf-tab-complete dummy-fzf-tab-widget
fzf_tab_before=${widgets[fzf-tab-complete]}
source "${0:A:h:h:h}/zshctl.zsh"
[[ $ZSHCTL_BOOTSTRAPPED == 1 ]]
[[ -n $ZSHCTL_SESSION_ID ]]
[[ ${widgets[fzf-tab-complete]} == $fzf_tab_before ]]
for function_name in zshctl-server zshctl-init zshctl-ensure-loaded zshctl-preload \
  zshctl-register-lazy-widget zshctl-register-lazy-widgets zshctl-bind-default-keys \
  zshctl-call-client-and-fallback zshctl-history-hooks zshctl-preprompt-hooks \
  zshctl-lazy-widget-dispatch zshctl-enable-sock; do
  (( $+functions[$function_name] ))
done
for widget in zshctl-auto-snippet zshctl-completion zshctl-history-selection \
  zshctl-insert-snippet zshctl-snippet-next-placeholder zshctl-ghq-cd; do
  [[ -n ${widgets[$widget]-} ]]
done

# Tab opens the snippet picker for matching line-start input and replaces the
# typed keyword without changing the buffer when fzf is cancelled.
(
  fake_root=$(mktemp -d)
  fake_zshctl=$fake_root/zshctl
  fake_fzf=$fake_root/fzf
  fake_fzf_args=$fake_root/fzf-args
  cat >"$fake_zshctl" <<'EOF'
#!/bin/sh
case "$*" in
  *--mode=completion*)
    printf 'failure\n'
    ;;
  *--mode=snippet-candidates*)
    printf 'success\n'
    printf '%s\n' '--filter=trusted --no-multi'
    printf '%s\n' 's0001-0000000000000001	git status gs	git status:  git status  [gs]'
    printf '%s\n' 's0002-0000000000000002	cd	cd:  cd /tmp'
    ;;
  *--mode=insert-snippet-id*)
    printf 'success\n'
    printf 'git status \n'
    printf '11\n'
    ;;
  *)
    exit 1
    ;;
esac
EOF
  cat >"$fake_fzf" <<'EOF'
#!/bin/sh
: >"$FAKE_FZF_ARGS"
printf '%s\n' "$@" >"$FAKE_FZF_ARGS"
printf 'env:FZF_DEFAULT_OPTS=%s\n' "${FZF_DEFAULT_OPTS-<unset>}" >>"$FAKE_FZF_ARGS"
printf 'env:FZF_DEFAULT_OPTS_FILE=%s\n' "${FZF_DEFAULT_OPTS_FILE-<unset>}" >>"$FAKE_FZF_ARGS"
[ "${FAKE_FZF_CANCEL:-0}" = 1 ] && exit 130
[ "${FAKE_FZF_NO_MATCH:-0}" = 1 ] && exit 1
[ "${FAKE_FZF_ERROR:-0}" = 1 ] && exit 2
[ "${FAKE_FZF_EMPTY:-0}" = 1 ] && exit 0
IFS= read -r selected || exit 1
printf '%s\n' "${FAKE_FZF_KEY:-}" "$selected"
EOF
  chmod +x "$fake_zshctl" "$fake_fzf"
  PATH="$fake_root:$PATH"
  export PATH ZSHCTL_FZF_COMMAND="$fake_fzf" FAKE_FZF_ARGS="$fake_fzf_args"
  export FZF_DEFAULT_OPTS='--select-1 --filter=must-not-match'
  last_call=
  calls=()
  zle() { calls+=("$*"); last_call="$*"; }
  # shellcheck disable=SC2034
  ZSHCTL_COMPLETION_FALLBACK=expand-or-complete
  autoload +X zshctl-completion
  trap 'rm -rf "$fake_root"' EXIT

  BUFFER=gs
  CURSOR=2
  LBUFFER=gs
  RBUFFER=
  zshctl-completion
  [[ $BUFFER == 'git status ' ]]
  [[ $CURSOR == 11 ]]
  grep -qx -- '--query=gs' "$fake_fzf_args"
  grep -qx -- '--filter=trusted' "$fake_fzf_args"
  grep -qx -- '--nth=1' "$fake_fzf_args"
  grep -qx -- '--with-nth=3' "$fake_fzf_args"
  grep -qx -- '--exit-0' "$fake_fzf_args"
  grep -qx -- '--no-select-1' "$fake_fzf_args"
  grep -qx 'env:FZF_DEFAULT_OPTS=<unset>' "$fake_fzf_args"
  grep -qx 'env:FZF_DEFAULT_OPTS_FILE=<unset>' "$fake_fzf_args"

  last_call=
  export FAKE_FZF_CANCEL=1
  BUFFER=gs
  CURSOR=2
  LBUFFER=gs
  RBUFFER=
  zshctl-completion
  [[ $BUFFER == gs ]]
  [[ $CURSOR == 2 ]]
  [[ $last_call == reset-prompt ]]
  unset FAKE_FZF_CANCEL

  export FAKE_FZF_NO_MATCH=1
  last_call=
  BUFFER=gs
  CURSOR=2
  LBUFFER=gs
  RBUFFER=
  zshctl-completion
  [[ $BUFFER == gs && $CURSOR == 2 ]]
  [[ $last_call == expand-or-complete ]]
  unset FAKE_FZF_NO_MATCH

  export FAKE_FZF_ERROR=1
  last_call=
  BUFFER=gs
  CURSOR=2
  LBUFFER=gs
  RBUFFER=
  zshctl-completion
  [[ $BUFFER == gs && $CURSOR == 2 ]]
  [[ $last_call == expand-or-complete ]]
  unset FAKE_FZF_ERROR

  export FAKE_FZF_EMPTY=1
  last_call=
  BUFFER=gs
  CURSOR=2
  LBUFFER=gs
  RBUFFER=
  zshctl-completion
  [[ $BUFFER == gs && $CURSOR == 2 ]]
  [[ $last_call == expand-or-complete ]]
  unset FAKE_FZF_EMPTY

  export FAKE_FZF_KEY=tab
  last_call=
  zshctl-completion
  [[ $BUFFER == gs && $CURSOR == 2 ]]
  [[ $last_call == expand-or-complete ]]
  unset FAKE_FZF_KEY FZF_DEFAULT_OPTS

  autoload +X zshctl-insert-snippet
  calls=()
  BUFFER='  gs'
  CURSOR=4
  LBUFFER='  gs'
  RBUFFER=
  zshctl-insert-snippet
  [[ $BUFFER == '  git status ' ]]
  [[ $CURSOR == 13 ]]
  [[ ${#calls[@]} == 1 && ${calls[1]} == reset-prompt ]]
  grep -qx -- '--query=gs' "$fake_fzf_args"

  calls=()
  BUFFER=$'\n gs'
  CURSOR=4
  LBUFFER=$'\n gs'
  RBUFFER=
  zshctl-insert-snippet
  [[ ${#calls[@]} == 1 && ${calls[1]} == reset-prompt ]]
  if grep -q '^--query=' "$fake_fzf_args"; then
    exit 1
  fi

  calls=()
  export FAKE_FZF_KEY=tab
  BUFFER=gs
  CURSOR=2
  LBUFFER=gs
  RBUFFER=
  zshctl-insert-snippet
  [[ ${#calls[@]} == 1 && ${calls[1]} == expand-or-complete ]]
  unset FAKE_FZF_KEY

  last_call=
  BUFFER=; LBUFFER=; RBUFFER=; CURSOR=0
  zshctl-completion
  [[ -z $BUFFER && $CURSOR == 0 ]]
  [[ $last_call == expand-or-complete ]]

  last_call=
  BUFFER='cd /tmp'
  CURSOR=7
  # shellcheck disable=SC2034
  LBUFFER='cd /tmp'
  # shellcheck disable=SC2034
  RBUFFER=
  zshctl-completion
  [[ $BUFFER == 'cd /tmp' ]]
  [[ $last_call == expand-or-complete ]]
)

# Enter must never fall back to self-insert and place a CR in the buffer.
(
  calls=()
  zle() { calls+=("$*"); }
  source "${0:A:h:h:h}/shells/zsh/widgets/zshctl-auto-snippet-and-accept-line"
  [[ $ZSHCTL_AUTO_SNIPPET_FALLBACK == _zshctl_noop ]]
  [[ ${calls[1]} == '-N _zshctl_noop' ]]
  [[ ${calls[2]} == zshctl-auto-snippet ]]
  [[ ${calls[3]} == accept-line ]]
)

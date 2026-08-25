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
dummy-fzf-tab-widget() { : }
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

# Enter must never fall back to self-insert and place a CR in the buffer.
(
  calls=()
  zle() { calls+=("$*") }
  source "${0:A:h:h:h}/shells/zsh/widgets/zshctl-auto-snippet-and-accept-line"
  [[ $ZSHCTL_AUTO_SNIPPET_FALLBACK == _zshctl_noop ]]
  [[ $calls[1] == '-N _zshctl_noop' ]]
  [[ $calls[2] == zshctl-auto-snippet ]]
  [[ $calls[3] == accept-line ]]
)

if [[ -n ${ZSHCTL_BOOTSTRAPPED-} ]]; then
  return 0
fi

() {
  emulate -L zsh
  local root=${${(%):-%x}:A:h:h:h}
  local widgets_dir=$root/shells/zsh/widgets
  local completions_dir=$root/shells/zsh/completions
  local widget

  export ZSHCTL_ROOT=${ZSHCTL_ROOT:-$root}
  export ZSHCTL_SESSION_ID=${ZSHCTL_SESSION_ID:-"zsh-${EPOCHREALTIME//./}-${$}-${RANDOM}"}
  export ZSHCTL_HISTORY_SESSION_ID=${ZSHCTL_HISTORY_SESSION_ID:-$ZSHCTL_SESSION_ID}
  export ZSHCTL_ENABLE=${ZSHCTL_ENABLE:-1}

  if (( ${path[(I)$ZSHCTL_ROOT/target/release]} == 0 )); then
    path+=("$ZSHCTL_ROOT/target/release")
  fi
  if (( ${fpath[(I)$widgets_dir]} == 0 )); then
    fpath+=("$widgets_dir")
  fi
  if (( ${fpath[(I)$completions_dir]} == 0 )); then
    fpath+=("$completions_dir")
  fi

  typeset -ga ZSHCTL_WIDGETS=(
    zshctl-auto-snippet zshctl-auto-snippet-and-accept-line zshctl-completion
    zshctl-ghq-cd zshctl-history-selection zshctl-insert-snippet
    zshctl-insert-space zshctl-preprompt zshctl-preprompt-snippet
    zshctl-smart-history-selection zshctl-snippet-next-placeholder
    zshctl-toggle-auto-snippet
  )

  for widget in $ZSHCTL_WIDGETS; do
    autoload -Uz -- "$widget"
    zle -N -- "$widget"
  done

  zshctl-server() { command zshctl server "$@" }
  zshctl-init() {
    [[ -n ${ZSHCTL_LOADED-} ]] && return 0
    if [[ ${ZSHCTL_DISABLE_DAEMON-} == (1|true) ]]; then
      export ZSHCTL_LOADED=1
      return 0
    fi
    command zshctl server start >/dev/null || return
    export ZSHCTL_LOADED=1
  }
  zshctl-ensure-loaded() { zshctl-init "$@" }
  zshctl-preload() { command zshctl server start >/dev/null 2>&1 &! }
  zshctl-enable-sock() { command zshctl server start >/dev/null }
  zshctl-call-client-and-fallback() { command zshctl "$@" }
  zshctl-register-lazy-widget() {
    local widget=$1
    autoload -Uz -- "$widget"
    zle -N -- "$widget"
  }
  zshctl-register-lazy-widgets() {
    local widget
    for widget in "$@"; do zshctl-register-lazy-widget "$widget"; done
  }
  zshctl-run-lazy-fallback() { zle ${1:-expand-or-complete} }
  zshctl-lazy-widget-dispatch() { zle "$@" }
  zshctl-history-hooks() {
    emulate -L zsh
    autoload -Uz add-zsh-hook
    zshctl-history-preexec() {
      [[ -n ${ZSHCTL_HISTORY_SUPPRESS-} ]] && return 0
      typeset -g ZSHCTL_HISTORY_LAST_COMMAND=$1
      typeset -g ZSHCTL_HISTORY_LAST_PWD=$PWD
      (( ! $+modules[zsh/datetime] )) && zmodload zsh/datetime
      typeset -g ZSHCTL_HISTORY_LAST_STARTED_AT=$EPOCHREALTIME
    }
    zshctl-history-precmd() {
      local exit_status=$?
      [[ -n ${ZSHCTL_HISTORY_LAST_COMMAND-} ]] || return 0
      [[ -n ${ZSHCTL_HISTORY_SUPPRESS-} ]] && return 0
      typeset -g ZSHCTL_HISTORY_SUPPRESS=1
      local started_at=${ZSHCTL_HISTORY_LAST_STARTED_AT-}
      local finished_at=$EPOCHREALTIME
      local seconds=${started_at%%.*}
      local fraction=${started_at#*.}
      fraction=${fraction:0:3}
      fraction=${(l:3::0:)fraction}
      local iso=$(command date -u -r "${seconds:-$EPOCHSECONDS}" '+%Y-%m-%dT%H:%M:%S' 2>/dev/null)
      [[ -n $iso ]] || iso=$(command date -u -d "@${seconds:-$EPOCHSECONDS}" '+%Y-%m-%dT%H:%M:%S' 2>/dev/null)
      local -F 10 duration=$(( (finished_at - started_at) * 1000 ))
      (( duration < 0 )) && duration=0
      command zshctl history log "$ZSHCTL_HISTORY_LAST_COMMAND" \
        --cwd "${ZSHCTL_HISTORY_LAST_PWD:-$PWD}" --exit-status "$exit_status" \
        --session "$ZSHCTL_HISTORY_SESSION_ID" --shell "${ZSH_NAME:-zsh}" \
        --host "${HOST:-}" --user "${USER:-}" --ts "${iso}.${fraction}Z" \
        --duration-ms "${duration%.*}" \
        >/dev/null 2>&1 &!
      unset ZSHCTL_HISTORY_SUPPRESS ZSHCTL_HISTORY_LAST_COMMAND ZSHCTL_HISTORY_LAST_PWD ZSHCTL_HISTORY_LAST_STARTED_AT
    }
    add-zsh-hook -d preexec zshctl-history-preexec 2>/dev/null
    add-zsh-hook -d precmd zshctl-history-precmd 2>/dev/null
    add-zsh-hook preexec zshctl-history-preexec
    add-zsh-hook precmd zshctl-history-precmd
    typeset -g ZSHCTL_HISTORY_HOOK_INITIALIZED=1
  }
  zshctl-preprompt-hooks() {
    emulate -L zsh
    [[ ${ZSHCTL_PREPROMPT_HOOK_INITIALIZED-} == 1 ]] && return 0
    autoload -Uz add-zle-hook-widget
    zshctl-preprompt-line-init() {
      [[ -n ${ZSHCTL_PREPROMPT_BUFFER-} && -z $BUFFER ]] || return 0
      BUFFER=$ZSHCTL_PREPROMPT_BUFFER
      CURSOR=${ZSHCTL_PREPROMPT_CURSOR:-${#BUFFER}}
    }
    add-zle-hook-widget line-init zshctl-preprompt-line-init
    typeset -g ZSHCTL_PREPROMPT_HOOK_INITIALIZED=1
  }

  zshctl-bind-default-keys() {
    bindkey ' ' zshctl-auto-snippet
    bindkey '^M' zshctl-auto-snippet-and-accept-line
    bindkey '^I' zshctl-completion
    bindkey '^R' zshctl-history-selection
    bindkey '^X^S' zshctl-insert-snippet
    bindkey '^X^G' zshctl-ghq-cd
  }
  zshctl-history-hooks
  [[ -o interactive ]] && zshctl-preprompt-hooks
  if (( $+functions[compdef] )); then
    autoload -Uz _zshctl
    compdef _zshctl zshctl zshctld
  fi

  export ZSHCTL_BOOTSTRAPPED=1
}

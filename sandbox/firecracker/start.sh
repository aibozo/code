#!/usr/bin/env bash
set -euo pipefail

# Minimal shim to enforce read-only host FS outside the project and optionally
# block outbound network operations. Intended as a stopgap until a real microVM
# runner is wired. Accepts policy args, then `--`, followed by the command.
#
#   --cwd <path>         working directory for the command
#   --writable <path>    explicit writable project root (may repeat)
#   --network on|off     allow or deny outbound network by default (off)

CWD="$(pwd)"
NETWORK="off"
WRITABLES=()
CMD=()

parse_args() {
  local mode="opts"
  while [[ $# -gt 0 ]]; do
    case "$mode:$1" in
      opts:--cwd)
        CWD="$2"; shift 2;;
      opts:--writable)
        WRITABLES+=("$2"); shift 2;;
      opts:--network)
        NETWORK="$2"; shift 2;;
      opts:--)
        mode="cmd"; shift;;
      cmd:*)
        CMD+=("$1"); shift;;
      *)
        shift;;
    esac
  done
}

inside_writable() {
  local p="$1"
  # Resolve relative paths against CWD; tolerate non-existent paths
  if [[ "$p" != /* ]]; then p="$CWD/$p"; fi
  local abs
  if abs=$(realpath -m -- "$p" 2>/dev/null); then
    for w in "${WRITABLES[@]}"; do
      local wabs
      if wabs=$(realpath -m -- "$w" 2>/dev/null); then
        case "$abs" in
          "$wabs"|"$wabs"/*) return 0;;
        esac
      fi
    done
  fi
  return 1
}

is_network_tool() {
  # Decide if the command is likely to require outbound network based on program and args.
  # Prefer false negatives to avoid blocking read-only operations.
  local prog="$1"; shift || true
  prog="${prog##*/}"
  case "$prog" in
    curl|wget)
      return 0;;
    git)
      # Only network for clone/fetch/pull/submodule
      local sub="${1:-}"
      case "$sub" in
        clone|fetch|pull|submodule|remote) return 0;;
        *) return 1;;
      esac
      ;;
    npm|pnpm|yarn)
      # Network for install/add/update/upgrade
      for a in "$@"; do
        case "$a" in install|add|update|upgrade) return 0;; esac
      done
      return 1;;
    pip|pip3|uv)
      case "${1:-}" in install|download|wheel) return 0;; esac
      return 1;;
    *)
      return 1;;
  esac
}

is_system_level_tool() {
  local p="$1"; p="${p##*/}"
  case "$p" in
    sudo|mount|umount|mkfs|mkfs.*|swapon|swapoff|shutdown|reboot|halt|poweroff|sysctl|modprobe|insmod|rmmod|iptables|ip6tables|ufw|nmcli|systemctl|docker|podman)
      return 0;;
    *) return 1;;
  esac
}

looks_destructive_outside_project() {
  # Heuristic: detect writes to absolute system paths or redirections to abs paths
  local -a argv=("$@")
  local prog="${argv[0]##*/}"

  # Block obvious system-level tools outright
  if is_system_level_tool "$prog"; then
    echo "reason=system-level tool blocked: $prog"; return 0
  fi

  case "$prog" in
    rm)
      for a in "${argv[@]:1}"; do
        if [[ "$a" == "/" || "$a" == "/*" ]]; then echo "reason=rm root"; return 0; fi
        if [[ "$a" == /* ]] && ! inside_writable "$a"; then echo "reason=rm outside project: $a"; return 0; fi
      done
      ;;
    mv|cp|mkdir|touch|install|rsync)
      local last="${argv[-1]}"
      if [[ "$last" == /* ]] && ! inside_writable "$last"; then
        echo "reason=$prog outside project: $last"; return 0
      fi
      ;;
    ln)
      # link path is last arg
      local last="${argv[-1]}"
      if [[ "$last" == /* ]] && ! inside_writable "$last"; then
        echo "reason=ln outside project: $last"; return 0
      fi
      ;;
    chmod|chown)
      local last="${argv[-1]}"
      if [[ "$last" == /* ]] && ! inside_writable "$last"; then
        echo "reason=$prog outside project: $last"; return 0
      fi
      ;;
    sed)
      for a in "${argv[@]:1}"; do
        if [[ "$a" == -i* ]]; then
          local t="${argv[-1]}"
          if [[ "$t" == /* ]] && ! inside_writable "$t"; then
            echo "reason=sed -i outside project: $t"; return 0
          fi
        fi
      done
      ;;
    dd)
      for a in "${argv[@]:1}"; do
        if [[ "$a" == of=* ]]; then
          local of="${a#of=}"
          if [[ "$of" == /* ]] && ! inside_writable "$of"; then
            echo "reason=dd of outside project: $of"; return 0
          fi
          if [[ "$of" == /dev/* ]]; then
            echo "reason=dd to block device: $of"; return 0
          fi
        fi
      done
      ;;
  esac
  # Shell redirection > file (also detect 2> etc.)
  for ((i=0; i<${#argv[@]}; i++)); do
    if [[ "${argv[$i]}" =~ ^([0-9]?>|>>)$ ]]; then
      local t="${argv[$((i+1))]:-}"
      if [[ -n "$t" && "$t" == /* ]] && ! inside_writable "$t"; then
        echo "reason=redirection outside project: $t"; return 0
      fi
    fi
  done
  return 1
}

main() {
  parse_args "$@"
  # Allow common temp directories by default for tooling that writes to TMPDIR
  if [[ -d /tmp ]]; then WRITABLES+=("/tmp"); fi
  if [[ -n "${TMPDIR:-}" && -d "$TMPDIR" ]]; then WRITABLES+=("$TMPDIR"); fi
  echo "[sandbox:firecracker] starting microVM (shim)"
  echo "[sandbox:firecracker] cwd=$CWD network=$NETWORK writable=${WRITABLES[*]}"
  if [[ ${#CMD[@]} -eq 0 ]]; then
    echo "[sandbox:firecracker] no command provided" >&2
    exit 1
  fi

  # Network policy
  if [[ "$NETWORK" == "off" ]] && is_network_tool "${CMD[@]}"; then
    echo "[sandbox:firecracker] network disabled: blocking '${CMD[0]##*/}'" >&2
    exit 113
  fi

  # Write policy (read-only outside writable roots)
  if reason=$(looks_destructive_outside_project "${CMD[@]}"); then
    echo "[sandbox:firecracker] denied by policy: $reason" >&2
    exit 114
  fi

  cd "$CWD"
  exec "${CMD[@]}"
}

main "$@"

#!/usr/bin/env bash
args=()
for arg in "$@"; do
  case "$arg" in
    --fix-cortex-a53-843419|-Wl,--fix-cortex-a53-843419)
      ;;
    *)
      args+=("$arg")
      ;;
  esac
done
exec zig cc -target "$ZIG_TARGET" "${args[@]}"

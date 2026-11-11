#!/usr/bin/env bash
args=()
for arg in "$@"; do
  case "$arg" in
    --target=*)
      ;;
    *)
      args+=("$arg")
      ;;
  esac
done
exec zig cc -target "$ZIG_TARGET" "${args[@]}"

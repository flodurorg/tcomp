#!/usr/bin/env bash
set -u

mods=(parser lexer relay session viewer config pty protocol terminal websocket)
dim=$'\033[90m'; grn=$'\033[32m'; cyn=$'\033[36m'; bld=$'\033[1m'; rst=$'\033[0m'

i=0
while true; do
  printf '%s%scargo test%s --workspace\n\n' "$dim" "$bld" "$rst"
  for m in "${mods[@]:0:3}"; do
    printf '%s   Compiling%s tcomp::%s\n' "$dim" "$rst" "$m"
    sleep 0.4
  done
  printf '\n%srunning %d tests%s\n' "$bld" "${#mods[@]}" "$rst"
  for m in "${mods[@]}"; do
    i=$((i + 1))
    printf '  %s✓%s tcomp::%-22s %s0.0%ds%s\n' "$grn" "$rst" "$m" "$dim" "$((i % 7))" "$rst"
    sleep 0.32
  done
  printf '\n%s%stest result: ok%s. %d passed; 0 failed\n\n' "$grn" "$bld" "$rst" "${#mods[@]}"
  sleep 1.2
done

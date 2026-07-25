#!/bin/sh
set -eu

file=/run/locho/container-stats.csv
project=${COLLECT_PROJECT:?COLLECT_PROJECT must be set}
printf 'timestamp,container,cpu_percent,memory_usage,memory_limit,pids,restarts,status,exit_code,open_fds,tasks,connections\n' > "$file"
while :; do
  timestamp=$(date -u +%FT%T%z)
  ids=$(docker ps -aq --filter "label=com.docker.compose.project=$project")
  for id in $ids; do
    stats=$(docker stats --no-stream --format '{{.Name}},{{.CPUPerc}},{{.MemUsage}},{{.PIDs}}' "$id" 2>/dev/null || true)
    [ -n "$stats" ] || continue
    lifecycle=$(docker inspect --format '{{.Name}},{{.RestartCount}},{{.State.Status}},{{.State.ExitCode}}' "$id" 2>/dev/null || true)
    [ -n "$lifecycle" ] || continue
    process=$(docker exec "$id" sh -c 'fd=$(ls /proc/1/fd 2>/dev/null | wc -l); tasks=$(ls /proc 2>/dev/null | awk "/^[0-9]+$/ {n++} END {print n+0}"); connections=$(awk "NR > 1 {n++} END {print n+0}" /proc/net/tcp 2>/dev/null); printf "%s,%s,%s" "$fd" "$tasks" "${connections:-0}"' 2>/dev/null || printf ',,')
    name=${lifecycle%%,*}
    lifecycle=${lifecycle#*,}
    printf '%s,%s,%s\n' "$timestamp" "$name" "$(printf '%s' "$stats" | cut -d, -f2-),$lifecycle,$process" >> "$file"
  done
  sleep "${COLLECT_INTERVAL:-5}"
done

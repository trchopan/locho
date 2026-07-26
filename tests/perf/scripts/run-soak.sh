#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
REPO=$(CDPATH= cd -- "$ROOT/../.." && pwd)
COMPOSE="docker compose -f $ROOT/compose.yaml"
DURATION=3600
HTTP_CONCURRENCY=10
TCP_CONCURRENCY=10
HTTP_SIZE=1024
HTTP_REQUEST_SIZE=0
TCP_SIZE=256
OUTPUT_ROOT="$REPO/artifacts"
CHURN_INTERVAL=60
SUCCESS_SAMPLE_RATE=100
INTERVAL_SECONDS=10
MIN_DURATION=360

usage() {
  echo "usage: $0 [--duration 1h] [--http-concurrency N] [--tcp-concurrency N] [--http-size BYTES] [--http-request-size BYTES] [--tcp-size BYTES] [--churn-interval SECONDS] [--success-sample-rate N] [--interval SECONDS] [--output DIR]"
}

parse_duration() {
  case "$1" in
    *h) awk "BEGIN {print ${1%h} * 3600}";;
    *m) awk "BEGIN {print ${1%m} * 60}";;
    *s) echo "${1%s}";;
    *) echo "$1";;
  esac
}

while [ $# -gt 0 ]; do
  case "$1" in
    --duration) DURATION=$(parse_duration "$2"); shift 2;;
    --http-concurrency) HTTP_CONCURRENCY=$2; shift 2;;
    --tcp-concurrency) TCP_CONCURRENCY=$2; shift 2;;
    --http-size) HTTP_SIZE=$2; shift 2;;
    --http-request-size) HTTP_REQUEST_SIZE=$2; shift 2;;
    --tcp-size) TCP_SIZE=$2; shift 2;;
    --churn-interval) CHURN_INTERVAL=$2; shift 2;;
    --success-sample-rate) SUCCESS_SAMPLE_RATE=$2; shift 2;;
    --interval) INTERVAL_SECONDS=$2; shift 2;;
    --output) OUTPUT_ROOT=$2; shift 2;;
    -h|--help) usage; exit 0;;
    *) usage >&2; exit 2;;
  esac
done

case "$DURATION" in *.*|*[!0-9]*) echo "duration must be an integer number of seconds or use h/m/s" >&2; exit 2;; esac
[ "$DURATION" -ge "$MIN_DURATION" ] || {
  echo "duration must be at least ${MIN_DURATION} seconds (6m)" >&2
  exit 2
}
[ "$DURATION" -le 3600 ] || { echo "duration must not exceed 1 hour" >&2; exit 2; }
for value in "$HTTP_CONCURRENCY" "$TCP_CONCURRENCY" "$HTTP_SIZE" "$HTTP_REQUEST_SIZE" "$TCP_SIZE" "$CHURN_INTERVAL" "$SUCCESS_SAMPLE_RATE" "$INTERVAL_SECONDS"; do
  case "$value" in *.*|*[!0-9]*) echo "numeric options must be non-negative integers" >&2; exit 2;; esac
done
[ "$HTTP_CONCURRENCY" -gt 0 ] || { echo "HTTP concurrency must be positive" >&2; exit 2; }
[ "$TCP_CONCURRENCY" -gt 0 ] || { echo "TCP concurrency must be positive" >&2; exit 2; }
[ "$CHURN_INTERVAL" -gt 0 ] || { echo "churn interval must be positive" >&2; exit 2; }
[ "$SUCCESS_SAMPLE_RATE" -gt 0 ] || { echo "success sample rate must be positive" >&2; exit 2; }
[ "$INTERVAL_SECONDS" -gt 0 ] || { echo "interval must be positive" >&2; exit 2; }

mkdir -p "$OUTPUT_ROOT"
OUTPUT_ROOT=$(CDPATH= cd -- "$OUTPUT_ROOT" && pwd)
RUN="$OUTPUT_ROOT/soak-$(date -u +%Y%m%d-%H%M%S)-$$"
RUNTIME="$RUN/runtime"
mkdir -p "$RUNTIME/host/state" "$RUNTIME/upstream" "$RUNTIME/http-client" "$RUNTIME/tcp-client" "$RUNTIME/loadgen" "$RUNTIME/collector"
export PERF_RUNTIME_DIR="$RUNTIME"
export SOAK_RUST_LOG=warn SOAK_UPSTREAM_QUIET=1
PROJECT_NAME="locho-soak-$$"
NETWORK_NAME="locho-soak-network-$$"
export COMPOSE_PROJECT_NAME="$PROJECT_NAME" PERF_NETWORK_NAME="$NETWORK_NAME"
COMPOSE="$COMPOSE --project-name $PROJECT_NAME"
START=$(date -u +%FT%T%z)
DEADLINE=$(( $(date +%s) + DURATION ))
WATCHDOG_PID=""
LOADGEN_PIDS=""
SOAK_FAILURE=0

cleanup() {
  status=$?
  set +e
  [ -n "$WATCHDOG_PID" ] && kill "$WATCHDOG_PID" 2>/dev/null
  for pid in $LOADGEN_PIDS; do kill "$pid" 2>/dev/null; done
  $COMPOSE logs --no-color > "$RUN/compose.log" 2>&1
  for service in locho_host locho_client_http locho_client_tcp upstream_http upstream_tcp loadgen collector; do
    $COMPOSE logs --no-color "$service" > "$RUN/$(printf '%s' "$service" | tr '_' '-').log" 2>&1
  done
  [ -f "$RUNTIME/collector/container-stats.csv" ] && cp "$RUNTIME/collector/container-stats.csv" "$RUN/container-stats.csv"
  if [ ! -s "$RUN/loadgen-events.jsonl" ]; then
    cat "$RUNTIME/loadgen"/*.jsonl > "$RUN/loadgen-events.jsonl" 2>/dev/null || :
  fi
  cp "$RUNTIME/loadgen"/*.json "$RUN/" 2>/dev/null || :
  [ -f "$RUN/timeline.jsonl" ] || : > "$RUN/timeline.jsonl"
  [ -f "$RUN/loadgen-events.jsonl" ] || : > "$RUN/loadgen-events.jsonl"
  image_metadata=""
  for service in locho_host locho_client_http locho_client_tcp upstream_http upstream_tcp loadgen collector; do
    image_metadata="$image_metadata\nimage_${service}=$($COMPOSE images -q "$service" 2>/dev/null)"
  done
  $COMPOSE down --volumes --remove-orphans >/dev/null 2>&1
  docker network rm "$NETWORK_NAME" >/dev/null 2>&1
  END=$(date -u +%FT%T%z)
  {
    echo "git_commit=$(git -C "$REPO" rev-parse HEAD)"
    echo "locho_version=$(awk -F '"' '/^version =/ {print $2; exit}' "$REPO/Cargo.toml")"
    echo "architecture=$(uname -m)"
    echo "docker=$(docker version --format '{{.Server.Version}}' 2>/dev/null)"
    echo "docker_compose=$($COMPOSE version 2>/dev/null)"
    printf '%b\n' "$image_metadata"
    echo "duration_seconds=$DURATION"
    echo "http_concurrency=$HTTP_CONCURRENCY tcp_concurrency=$TCP_CONCURRENCY"
    echo "http_size=$HTTP_SIZE http_request_size=$HTTP_REQUEST_SIZE tcp_size=$TCP_SIZE churn_interval=$CHURN_INTERVAL success_sample_rate=$SUCCESS_SAMPLE_RATE interval_seconds=$INTERVAL_SECONDS"
    echo "start=$START end=$END deadline=$DEADLINE exit_status=$status"
  } > "$RUN/metadata.txt"
  rm -rf "$RUNTIME"
  exit "$status"
}
trap cleanup EXIT INT TERM

(
  sleep "$DURATION"
  kill -TERM "$$" 2>/dev/null
) &
WATCHDOG_PID=$!

record_event() {
  python3 - "$RUN/timeline.jsonl" "$1" "$2" "$3" "$4" "$5" <<'PY'
import json
import sys
import time

path, event, service, start, deadline, result = sys.argv[1:]
with open(path, "a") as output:
    json.dump({
        "ts": time.time(),
        "event": event,
        "service": service,
        "started_at": start,
        "recovery_deadline": deadline,
        "result": result,
    }, output)
    output.write("\n")
PY
}

openssl req -x509 -newkey rsa:2048 -nodes -days 2 -subj /CN=locho-soak-ca \
  -keyout "$RUNTIME/upstream/ca.key" -out "$RUNTIME/upstream/ca.crt" >/dev/null 2>&1
openssl req -new -newkey rsa:2048 -nodes -subj /CN=upstream_http \
  -keyout "$RUNTIME/upstream/server.key" -out "$RUNTIME/upstream/server.csr" >/dev/null 2>&1
printf 'subjectAltName=DNS:upstream_http,IP:172.30.0.20\n' > "$RUNTIME/upstream/server.ext"
openssl x509 -req -in "$RUNTIME/upstream/server.csr" -CA "$RUNTIME/upstream/ca.crt" \
  -CAkey "$RUNTIME/upstream/ca.key" -CAcreateserial -days 2 \
  -extfile "$RUNTIME/upstream/server.ext" -out "$RUNTIME/upstream/server.crt" >/dev/null 2>&1
cp "$RUNTIME/upstream/ca.crt" "$RUNTIME/host/ca.crt"
cat > "$RUNTIME/host/locho.toml" <<EOF
[[services]]
name = "api"
type = "http"
upstream = "https://upstream_http:8443"
ca_cert = "/run/locho/ca.crt"

[[services]]
name = "echo"
type = "tcp"
endpoint = "172.30.0.21:9000"
EOF
cp "$ROOT/scripts/start-http-client.sh" "$RUNTIME/http-client/start-http-client.sh"
cp "$ROOT/scripts/start-tcp-client.sh" "$RUNTIME/tcp-client/start-tcp-client.sh"
cp "$ROOT/scripts/collector.sh" "$RUNTIME/collector/collector.sh"
chmod +x "$RUNTIME/http-client/start-http-client.sh" "$RUNTIME/tcp-client/start-tcp-client.sh" "$RUNTIME/collector/collector.sh"

$COMPOSE up -d --build --wait locho_host upstream_http upstream_tcp >/dev/null
for _ in $(seq 1 60); do [ -s "$RUNTIME/host/state/host_state.json" ] && break; sleep 1; done
[ -s "$RUNTIME/host/state/host_state.json" ] || { echo "host did not become ready" >&2; exit 1; }
python3 - "$RUNTIME/host/state/host_state.json" "$RUNTIME" <<'PY'
import json
import sys

state = json.load(open(sys.argv[1]))
root = sys.argv[2]
for name, service in (("http", "api"), ("tcp", "echo")):
    directory = "http-client" if name == "http" else "tcp-client"
    with open(f"{root}/{directory}/{name}-attach", "w") as output:
        output.write(f"{state['endpoint_id']} {service} {state['service_secrets'][service]}\n")
PY
$COMPOSE up -d --build --no-deps locho_client_http locho_client_tcp loadgen >/dev/null
for _ in $(seq 1 60); do
  http_ready=$($COMPOSE exec -T locho_client_http sh -c "ss -ltn | grep -q ':8765 '" >/dev/null 2>&1; echo $?)
  tcp_ready=$($COMPOSE exec -T locho_client_tcp sh -c "ss -ltn | grep -q ':9876 '" >/dev/null 2>&1; echo $?)
  if [ "$http_ready" -eq 0 ] && [ "$tcp_ready" -eq 0 ]; then ready=1; break; fi
  sleep 1
done
[ "${ready:-0}" -eq 1 ] || { echo "attachments did not become ready" >&2; exit 1; }
$COMPOSE up -d --no-deps collector >/dev/null
$COMPOSE config > "$RUN/compose-config.yaml"

remaining=$((DEADLINE - $(date +%s) - 30))
[ "$remaining" -ge 90 ] || { echo "setup consumed the available soak budget" >&2; exit 2; }
warmup=$((remaining / 24)); [ "$warmup" -lt 5 ] && warmup=5
cooldown=$warmup
recovery=$((remaining / 6)); [ "$recovery" -lt 60 ] && recovery=60
# Each of the three sequential restart probes can consume the full 30-second
# recovery window. Reserve that time separately from the traffic phase.
recovery_overhead=90
recovery=$((recovery + recovery_overhead))
steady=$((remaining - warmup - cooldown - recovery))
[ "$steady" -gt 0 ] || {
  echo "duration leaves no steady-state budget after setup and recovery reservations" >&2
  exit 2
}

run_loadgen() {
  protocol=$1; duration=$2; label=$3; concurrency=$4; size=$5
  request_args=""
  [ "$protocol" = http ] && request_args="--request-size $HTTP_REQUEST_SIZE"
  $COMPOSE exec -T loadgen python /opt/loadgen/loadgen.py \
    --protocol "$protocol" --duration "$duration" --concurrency "$concurrency" \
    --size "$size" $request_args --churn-interval "$CHURN_INTERVAL" \
    --success-sample-rate "$SUCCESS_SAMPLE_RATE" --interval "$INTERVAL_SECONDS" \
    --output "/run/locho/$label-$protocol.json" --events "/run/locho/$label-$protocol.jsonl" \
    > "$RUNTIME/loadgen/$label-$protocol.stdout" 2>&1
}

start_traffic() {
  duration=$1; label=$2
  run_loadgen http "$duration" "$label" "$HTTP_CONCURRENCY" "$HTTP_SIZE" &
  LOADGEN_PIDS="$LOADGEN_PIDS $!"
  run_loadgen tcp "$duration" "$label" "$TCP_CONCURRENCY" "$TCP_SIZE" &
  LOADGEN_PIDS="$LOADGEN_PIDS $!"
}

wait_traffic() {
  mode=$1
  failed=0
  set +e
  for pid in $LOADGEN_PIDS; do
    wait "$pid" || failed=1
  done
  set -e
  LOADGEN_PIDS=""
  if [ "$mode" = strict ] && [ "$failed" -ne 0 ]; then
    SOAK_FAILURE=1
  fi
}

probe() {
  protocol=$1
  label=$2
  end=$(( $(date +%s) + 30 ))
  [ "$end" -gt "$PROBE_DEADLINE" ] && end=$PROBE_DEADLINE
  while [ "$(date +%s)" -lt "$end" ] && [ "$(date +%s)" -lt "$DEADLINE" ]; do
    if [ "$protocol" = http ]; then size=$HTTP_SIZE; request_args="--request-size $HTTP_REQUEST_SIZE"; else size=$TCP_SIZE; request_args=""; fi
    if $COMPOSE exec -T loadgen python /opt/loadgen/loadgen.py --protocol "$protocol" --duration 1 \
      --concurrency 1 --size "$size" $request_args --output "/run/locho/probe-$label-$protocol.json" \
      --events "/run/locho/probe-$label-$protocol.jsonl" >/dev/null 2>&1 \
      && python3 - "$RUNTIME/loadgen/probe-$label-$protocol.json" <<'PY'
import json
import sys

try:
    with open(sys.argv[1]) as source:
        summary = json.load(source)
except (OSError, json.JSONDecodeError):
    raise SystemExit(1)
raise SystemExit(0 if summary.get("counts", {}).get("success", 0) > 0 else 1)
PY
    then return 0; fi
    sleep 1
  done
  return 1
}

recover() {
  service=$1; shift
  start=$(date -u +%FT%T%z)
  PROBE_DEADLINE=$(( $(date +%s) + 30 ))
  recovery_deadline=$(date -u -v+30S +%FT%T%z 2>/dev/null || date -u -d '+30 seconds' +%FT%T%z 2>/dev/null || date -u +%FT%T%z)
  record_event restart "$service" "$start" "$recovery_deadline" started
  result=failed
  if $COMPOSE restart "$service" >/dev/null 2>&1; then
    result=recovered
    probe_pids=""
    for protocol in "$@"; do
      probe "$protocol" "$service" &
      probe_pids="$probe_pids $!"
    done
    set +e
    for pid in $probe_pids; do
      wait "$pid" || result=failed
    done
    set -e
  fi
  record_event restart "$service" "$start" "$recovery_deadline" "$result"
  [ "$result" = recovered ]
}

start_traffic "$warmup" warmup
wait_traffic strict
start_traffic "$steady" steady
wait_traffic strict
RECOVERY_DEADLINE=$(( $(date +%s) + recovery ))
recovery_traffic=$((recovery - recovery_overhead)); [ "$recovery_traffic" -lt 1 ] && recovery_traffic=1
start_traffic "$recovery_traffic" recovery
if ! recover locho_host http tcp; then SOAK_FAILURE=1; fi
if ! recover locho_client_http http tcp; then SOAK_FAILURE=1; fi
if ! recover locho_client_tcp tcp http; then SOAK_FAILURE=1; fi
wait_traffic allow
start_traffic "$cooldown" cooldown
wait_traffic strict

: > "$RUN/loadgen-events.jsonl"
for events in "$RUNTIME/loadgen"/*.jsonl; do
  case "$events" in *probe-*) continue;; esac
  cat "$events" >> "$RUN/loadgen-events.jsonl" 2>/dev/null || :
done
python3 - "$RUN" "$DURATION" <<'PY'
import glob
import json
import sys

run, duration = sys.argv[1], int(sys.argv[2])
summaries = []
for path in glob.glob(run + "/runtime/loadgen/*.json"):
    if path.rsplit("/", 1)[-1].startswith("probe-"):
        continue
    try:
        summaries.append(json.load(open(path)))
    except (OSError, json.JSONDecodeError):
        pass
counts = {key: sum(item.get("counts", {}).get(key, 0) for item in summaries) for key in ("success", "failure", "timeout", "reset")}
with open(run + "/loadgen-summary.json", "w") as output:
    json.dump({"duration_seconds": duration, "runs": len(summaries), "counts": counts, "summaries": summaries}, output, indent=2)
PY
exit "$SOAK_FAILURE"

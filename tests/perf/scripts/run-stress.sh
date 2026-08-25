#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
REPO=$(CDPATH= cd -- "$ROOT/../.." && pwd)
COMPOSE="docker compose -f $ROOT/compose.yaml"
DURATION=900
HTTP_SIZE=1024
HTTP_REQUEST_SIZE=0
TCP_SIZE=256
OUTPUT_ROOT="$REPO/artifacts"
HTTP_LEVELS="1 10 50 100"
TCP_LEVELS="1 10 50"

usage() { echo "usage: $0 [--duration 15m] [--http-concurrency LIST] [--tcp-concurrency LIST] [--http-size BYTES] [--http-request-size BYTES] [--tcp-size BYTES] [--output DIR]"; }
parse_duration() {
  case "$1" in *h) awk "BEGIN {print ${1%h} * 3600}";; *m) awk "BEGIN {print ${1%m} * 60}";; *s) echo "${1%s}";; *) echo "$1";; esac
}
while [ $# -gt 0 ]; do
  case "$1" in
    --duration) DURATION=$(parse_duration "$2"); shift 2;;
    --http-concurrency) HTTP_LEVELS=$2; shift 2;;
    --tcp-concurrency) TCP_LEVELS=$2; shift 2;;
    --http-size) HTTP_SIZE=$2; shift 2;;
    --http-request-size) HTTP_REQUEST_SIZE=$2; shift 2;;
    --tcp-size) TCP_SIZE=$2; shift 2;;
    --output) OUTPUT_ROOT=$2; shift 2;;
    -h|--help) usage; exit 0;;
    *) usage >&2; exit 2;;
  esac
done

case "$DURATION" in *.*) exit 2;; esac
[ "$DURATION" -gt 0 ] || { echo "duration must be positive" >&2; exit 2; }
mkdir -p "$OUTPUT_ROOT"
OUTPUT_ROOT=$(CDPATH= cd -- "$OUTPUT_ROOT" && pwd)
RUN="$OUTPUT_ROOT/stress-$(date -u +%Y%m%d-%H%M%S)-$$"
RUNTIME="$RUN/runtime"
mkdir -p "$RUNTIME/host/state" "$RUNTIME/upstream" "$RUNTIME/http-client" "$RUNTIME/tcp-client" "$RUNTIME/loadgen" "$RUNTIME/collector" "$RUN"
export PERF_RUNTIME_DIR="$RUNTIME"
PROJECT_NAME="locho-stress-$$"
NETWORK_NAME="locho-stress-network-$$"
export COMPOSE_PROJECT_NAME="$PROJECT_NAME" PERF_NETWORK_NAME="$NETWORK_NAME"
COMPOSE="$COMPOSE --project-name $PROJECT_NAME"
START=$(date -u +%FT%T%z)
DEADLINE=$(( $(date +%s) + DURATION ))
cleanup() {
  status=$?
  set +e
  $COMPOSE logs --no-color > "$RUN/compose.log" 2>&1
  for service in locho_host locho_client_http locho_client_tcp upstream_http upstream_tcp loadgen collector; do
    $COMPOSE logs --no-color "$service" > "$RUN/$service.log" 2>&1
  done
  [ -f "$RUNTIME/collector/container-stats.csv" ] && cp "$RUNTIME/collector/container-stats.csv" "$RUN/container-stats.csv"
  image_metadata=""
  for service in locho_host locho_client_http locho_client_tcp upstream_http upstream_tcp loadgen collector; do
    image_metadata="$image_metadata\nimage_${service}=$($COMPOSE images -q "$service" 2>/dev/null)"
  done
  $COMPOSE down --volumes --remove-orphans >/dev/null 2>&1
  END=$(date -u +%FT%T%z)
  {
    echo "git_commit=$(git -C "$REPO" rev-parse HEAD)"
    echo "locho_version=$(awk -F '\"' '/^version =/ {print $2; exit}' "$REPO/Cargo.toml")"
    echo "architecture=$(uname -m)"
    echo "docker=$(docker version --format '{{.Server.Version}}' 2>/dev/null)"
    echo "docker_compose=$($COMPOSE version 2>/dev/null)"
    printf '%b\n' "$image_metadata"
    echo "duration_seconds=$DURATION"
    echo "http_size=$HTTP_SIZE http_request_size=$HTTP_REQUEST_SIZE tcp_size=$TCP_SIZE"
    echo "start=$START end=$END exit_status=$status"
  } > "$RUN/metadata.txt"
  rm -rf "$RUNTIME"
  exit "$status"
}
trap cleanup EXIT INT TERM

openssl req -x509 -newkey rsa:2048 -nodes -days 2 -subj /CN=locho-stress-ca -keyout "$RUNTIME/upstream/ca.key" -out "$RUNTIME/upstream/ca.crt" >/dev/null 2>&1
openssl req -new -newkey rsa:2048 -nodes -subj /CN=upstream_http -keyout "$RUNTIME/upstream/server.key" -out "$RUNTIME/upstream/server.csr" >/dev/null 2>&1
printf 'subjectAltName=DNS:upstream_http,IP:172.30.0.20\n' > "$RUNTIME/upstream/server.ext"
openssl x509 -req -in "$RUNTIME/upstream/server.csr" -CA "$RUNTIME/upstream/ca.crt" -CAkey "$RUNTIME/upstream/ca.key" -CAcreateserial -days 2 -extfile "$RUNTIME/upstream/server.ext" -out "$RUNTIME/upstream/server.crt" >/dev/null 2>&1
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
import json, sys
state = json.load(open(sys.argv[1]))
root = sys.argv[2]
for name, service in (("http", "api"), ("tcp", "echo")):
    directory = "http-client" if name == "http" else "tcp-client"
    with open(f"{root}/{directory}/{name}-attach", "w") as out:
        out.write(f"{state['endpoint_id']} {service} {state['service_secrets'][service]}\n")
PY
$COMPOSE up -d --build --no-deps locho_client_http locho_client_tcp loadgen >/dev/null
for _ in $(seq 1 60); do
  http_ready=$($COMPOSE exec -T locho_client_http sh -c "ss -ltn | grep -q ':8765 '" >/dev/null 2>&1; echo $?)
  tcp_ready=$($COMPOSE exec -T locho_client_tcp sh -c "ss -ltn | grep -q ':9876 '" >/dev/null 2>&1; echo $?)
  if [ "$http_ready" -eq 0 ] && [ "$tcp_ready" -eq 0 ]; then
    ready=1
    break
  fi
  sleep 1
done
[ "${ready:-0}" -eq 1 ] || { echo "attachments did not become ready" >&2; exit 1; }
$COMPOSE up -d --no-deps collector >/dev/null

$COMPOSE config > "$RUN/compose-config.yaml"

printf 'phase,protocol,kind,concurrency,duration_seconds,summary\n' > "$RUN/phases.csv"
phase=0
run_phase() {
  protocol=$1; kind=$2; concurrency=$3; duration=$4; size=$5
  [ "$duration" -gt 0 ] || return 0
  remaining=$((DEADLINE - $(date +%s)))
  [ "$remaining" -gt 0 ] || { echo "run deadline reached before $protocol $kind phase" >&2; return 1; }
  [ "$duration" -le "$remaining" ] || duration=$remaining
  phase=$((phase + 1)); output="$RUNTIME/loadgen/summary-$phase.json"; events="$RUNTIME/loadgen/events-$phase.jsonl"
  request_args=""
  [ "$protocol" = http ] && request_args="--request-size $HTTP_REQUEST_SIZE"
  if ! $COMPOSE exec -T loadgen python /opt/loadgen/loadgen.py --protocol "$protocol" --duration "$duration" --concurrency "$concurrency" --size "$size" $request_args --output "/run/locho/summary-$phase.json" --events "/run/locho/events-$phase.jsonl"; then
    cp "$output" "$RUN/summary-$phase.json" 2>/dev/null || true
    cp "$events" "$RUN/events-$phase.jsonl" 2>/dev/null || true
    return 1
  fi
  cp "$output" "$RUN/summary-$phase.json"
  cp "$events" "$RUN/events-$phase.jsonl"
  printf '%s,%s,%s,%s,%s,' "$phase" "$protocol" "$kind" "$concurrency" "$duration" >> "$RUN/phases.csv"
  tr '\n' ' ' < "$RUN/summary-$phase.json" >> "$RUN/phases.csv"; printf '\n' >> "$RUN/phases.csv"
}
count_levels() { set -- $1; echo $#; }
http_count=$(count_levels "$HTTP_LEVELS")
tcp_count=$(count_levels "$TCP_LEVELS")
phase_count=$((http_count + tcp_count + 4))
budget=$((DEADLINE - $(date +%s)))
[ "$budget" -ge "$phase_count" ] || { echo "remaining budget is too short for ${phase_count} phases" >&2; exit 2; }
warmup=$((budget / 10)); [ "$warmup" -gt 120 ] && warmup=120; [ "$warmup" -lt 1 ] && warmup=1
cooldown=$warmup
steady=$((budget - warmup * 4))
per_http=$((steady / (http_count + tcp_count)))
[ "$per_http" -gt 0 ] || per_http=1
run_levels() {
  protocol=$1; size=$2; levels=$3
  run_phase "$protocol" warmup 1 "$warmup" "$size"
  for level in $levels; do
    run_phase "$protocol" steady "$level" "$per_http" "$size"
  done
  run_phase "$protocol" cooldown 1 "$cooldown" "$size"
}
run_levels http "$HTTP_SIZE" "$HTTP_LEVELS"
run_levels tcp "$TCP_SIZE" "$TCP_LEVELS"
cat "$RUN"/events-*.jsonl > "$RUN/loadgen-events.jsonl"
python3 - "$RUN" "$DURATION" <<'PY'
import glob, json, sys
run, duration = sys.argv[1], int(sys.argv[2])
summaries = [json.load(open(path)) for path in glob.glob(run + "/summary-*.json")]
counts = {key: sum(item["counts"].get(key, 0) for item in summaries) for key in ("success", "failure", "timeout", "reset")}
with open(run + "/loadgen-summary.json", "w") as output:
    json.dump({"duration_seconds": duration, "phases": len(summaries), "counts": counts}, output, indent=2)
PY

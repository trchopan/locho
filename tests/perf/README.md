# Locho Stress Harness

This is a test-only Docker Compose harness for measuring the current checkout.
It never uses the production Compose project, `.env`, Caddy state, or release
artifacts. The Locho image is built from the repository source with the
`integration-test` feature so the fixed direct-address path can be exercised.

## Requirements

- Docker Desktop with Compose v2
- OpenSSL and Python 3 on the host
- Network access for iroh discovery/relay bootstrap

Apple Silicon hosts should use the native ARM64 Docker platform. The fixtures
and load generator are architecture-independent.

## Running

From the repository root:

```sh
docker compose -f tests/perf/compose.yaml config
./tests/perf/scripts/run-stress.sh --duration 2m
./tests/perf/scripts/run-stress.sh --duration 15m
./tests/perf/scripts/run-soak.sh --duration 30m
```

The duration is a total wall-clock budget. A short run scales each HTTP and TCP
concurrency matrix across the available budget while retaining both protocols.
The full matrix is used when the budget permits. Override levels and payloads
when isolating a result:

```sh
./tests/perf/scripts/run-stress.sh --duration 5m \
  --http-concurrency "1 10 50" --tcp-concurrency "1 10" \
  --http-size 4096 --http-request-size 512 --tcp-size 1024 --output artifacts
```

## Artifacts

Each run creates `artifacts/stress-<UTC timestamp>-<pid>/` containing metadata,
per-phase JSON summaries and JSONL events, phase CSV, container stats, and logs.
The exit trap collects these files and removes the Compose containers, network,
and temporary runtime state even after an interrupted run. The artifact
directory is retained for analysis.

The load generator records successful operations, failures with reasons,
timeouts, connection resets, throughput, and p50/p95/p99 latency. HTTP request
and response sizes, and TCP message sizes, are configurable. Events are streamed
to JSONL and latency samples are bounded so long runs do not retain every
operation in memory. Container statistics are sampled every five seconds by the
collector, including restart count and exit status. No throughput threshold is
imposed; compare runs using the same machine and commit.

The soak runner accepts `--http-timeout-secs` and writes that value into the
host configuration. Use a short value only with a deliberately delayed fixture;
the normal HTTPS fixture is expected to complete within the timeout.

The standard stress baseline stays below Locho's documented TCP connection
limit. Run `--tcp-concurrency "1 10 50 100"` separately when validating the
limit and expected rejection behavior.

## Soak Runs

The soak runner maintains mixed HTTP and TCP traffic, periodically churns
connections, and restarts the HTTP attachment, TCP attachment, and host after
traffic is established:

```sh
./tests/perf/scripts/run-soak.sh --duration 30m
./tests/perf/scripts/run-soak.sh --duration 1h
```

The duration is the total wall-clock budget and cannot exceed 1 hour. Soak
durations shorter than 6 minutes are rejected because the run reserves time for
setup, warmup, cooldown, and three sequential 30-second recovery probes. Soak
artifacts are written to `artifacts/soak-<UTC timestamp>-<pid>/`. Recovery
results are in `timeline.jsonl`; transient failures during a restart remain in
the load-generator events. Successful operations are sampled and interval
summaries are emitted every 10 seconds by default, keeping long-run artifacts
bounded. Failure events are capped per load-generator process while interval
counts continue to include every failure. Override this with
`--success-sample-rate` and `--interval` when needed. Compose does not simulate
macOS suspend/resume, so perform the lid-close/open check separately on the M1
Pro.

## Fixtures

`upstream_http` is a deterministic HTTPS server using a fresh run-specific CA
and certificate. It returns request metadata and a configurable response body.
`upstream_tcp` is a deterministic TCP echo server. Both are isolated on the
dedicated `locho-stress-network` network.

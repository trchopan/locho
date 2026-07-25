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

## Fixtures

`upstream_http` is a deterministic HTTPS server using a fresh run-specific CA
and certificate. It returns request metadata and a configurable response body.
`upstream_tcp` is a deterministic TCP echo server. Both are isolated on the
dedicated `locho-stress-network` network.

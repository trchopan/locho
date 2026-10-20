# Locho Stress Harness

## Native Unix stream smoke, stress and soak tests

`unix_streams.py` tests filesystem Unix stream support using the **ordinary
release binary**, native processes and generic byte-stream fixtures. It needs
Python 3.11+, `ps`, and (on macOS) `lsof`. There is no Herdr dependency and no
Docker requirement. Run it natively on both macOS and Linux: Docker Desktop
socket tests alone would exercise its Linux VM, not macOS listener behavior.

```sh
cargo build --release
python3 -m unittest discover -s tests/perf -p test_unix_streams.py -v
python3 tests/perf/unix_streams.py smoke
python3 tests/perf/unix_streams.py stress --duration 5m
python3 tests/perf/unix_streams.py soak --duration 30m
python3 tests/perf/unix_streams.py soak --duration 1h
```

To test another built binary, supply `--binary /absolute/path/to/locho` or
`LOCHO_TEST_BINARY`. The smoke test also participates in the existing Unix
release-smoke gate:

```sh
LOCHO_TEST_BINARY=target/release/locho cargo test --test release_smoke \
  release_binary_unix_stream_smoke -- --ignored --nocapture
```

### Smoke coverage

- TCP→TCP, TCP→Unix, Unix→TCP, Unix→Unix, with mixed listeners sharing a peer.
- Binary/checksummed frames, 0/1-byte, 16 KiB and 1 MiB payloads, fragmented
  writes, half-close followed by a complete response, concurrent clients, slow
  readers and simultaneous 2 MiB transfers in both directions.
- Mode `0600`, insecure-parent rejection, existing file/directory/symlink/live
  socket refusal, replacement-preserving cleanup and all-listener startup rollback.
- SIGKILL stale paths are refused until explicitly removed by the harness;
  Ctrl-C/SIGTERM clean up owned sockets, including with an active connection.
- SIGTERM during partial multi-listener startup and abrupt local Unix client
  reset without disconnecting another service on the shared peer.
- Wrong capabilities, unavailable/restarted Unix backend, host and attachment
  restart, unchanged listener inode during backend/host recovery, fresh probes
  within 30 seconds, and shared host-budget rejection at 128 active streams.
- One-second host/attachment idle policies independently enforce expiry.

The budget probe uses multiple peers because the transport's per-peer stream
window can be smaller than the host's application budget. Foreign-owner tests
requiring another UID are explicitly skipped, not reported as passed. Existing
HTTP/TLS, capability rotation and HTTP timeout regressions remain covered by the
other `release_smoke.rs` cases. Controlled-clock core tests cover the default
300-second deadline, bytes resetting it, EOF not resetting it and disabled-idle
half-close behavior. Native adapter unit tests supplement filesystem cases.

### Stress profile and acceptance

The stress profile ramps concurrent workers per forwarding combination through
4, 8, 16 and 32 by default over a five-minute load budget. Override the matrix
or use one level when isolating a result:

```sh
python3 tests/perf/unix_streams.py stress --duration 5m --levels 4,8,16,32
python3 tests/perf/unix_streams.py stress --duration 2m --concurrency 16
```

Each level exercises TCP→TCP, TCP→Unix, Unix→TCP and Unix→Unix with persistent
connections, churn, fragmented/slow transfers and periodic 1 MiB payloads.
Stress fails on corruption, replay, unexpected I/O failures, worker/process
failure, cleanup failure or descriptor growth above `--max-fd-growth`. It
records throughput and p50/p95/p99 latency plus RSS/CPU/FD samples; those
performance values are diagnostic and should be compared across identical
machine/platform runs. Fault restarts and long idle-expiry gates belong to the
soak profile.

The default stress duration is five minutes; accepted stress durations are one
minute to one hour. Artifacts use the
`artifacts/unix-stress-<timestamp>/` profile name.

### Soak profile and acceptance

The default is **30 minutes of load after setup**; accepted durations are
15 minutes to 1 hour. Setup, fault recovery and cleanup have separate bounds;
an outer watchdog prevents an unbounded run. The 15-minute minimum reserves an
uninterrupted six-minute quiet gate before fault injection.

Three workers per combination maintain persistent traffic, one-connection-per-
operation churn, periodic 1 MiB transfers and delayed readers. Each worker has
its own identity and monotonically increasing sequence; a failed operation is
never retried. Fixtures validate both request and response contents and retain
bounded high-water marks across upstream restarts to detect replay/duplicates.
Normal traffic remains below the host's connection budget.

Four separate connections remain application-byte quiet for **365 seconds**
with both endpoint idle policies disabled; they must then exchange a fresh
frame on the same connection. A separate default-policy connection must close
between 290 and 320 seconds. Keepalives are not application bytes. At 45%, 65%
and 80% of the load duration, restart the Unix backend, Locho host (preserving
state), and attachment respectively. The first two faults must not replace the
attachment listener. Existing streams fail without replay; only fresh probes
are retried during recovery.

A run fails on corruption, replay, unexpected steady-state I/O failure, an
unexpected process exit, failed recovery, failed quiet/default-expiry gates,
cleanup failure, or post-drain descriptor growth greater than 16 per Locho
process (`--max-fd-growth` overrides this allowance). Before any scheduled
restart, workers drain and descriptors are compared against the **same PID**
baseline; traffic then resumes. This checkpoint occurs after the quiet gate,
at the later of 370 seconds or 40% of load duration. Scheduled-fault failures
are counted separately and remain visible. Classification follows the affected
service's connection/operation epoch: an old persistent stream can discover its
terminal EOF after fresh probes recover. There is no blanket post-fault grace
period, and failures on fresh connections after recovery remain unexpected.
An upstream-only fault does not excuse failures on TCP-backed services. The
short smoke profile also runs traffic during restarts to cover this accounting.
RSS/CPU and descriptor history are
recorded; RSS/throughput are diagnostic rather than arbitrary pass thresholds.
Compare identical binary/platform profiles for leak and performance trends;
the final post-restart deltas are diagnostic only and cannot satisfy the
same-PID descriptor gate. Inspect within-PID RSS samples for monotonic growth.

### Native artifacts and isolation

Each run writes `artifacts/unix-{smoke,stress,soak}-<timestamp>/` (override the parent
with `--output`) containing:

- `summary.json`: outcome, binary version/hash, OS/Python, operation counts,
  fixture corruption/replay counts, bounded latency sample p50/p95/p99.
- `timeline.jsonl`: gates, scheduled faults, recovery times, and capped failure events.
- `metrics.jsonl` (stress/soak): RSS/CPU/FD samples and cumulative counters;
  byte-count deltas can be used to calculate interval throughput.
- Bounded process logs with generated credentials and runtime paths redacted.

Counters include every failure even after the 2,000-event cap; latency history
is bounded to the latest 10,000 samples. Credentials/configs/state stay in a
unique mode-0700 temporary directory, not the retained artifacts. Every child
has its own run-owned process handle; cleanup never stops unrelated sessions.
Unix listeners live below a short temporary directory rather than the checkout
path (macOS's limit is 103 bytes). If necessary, set `TMPDIR` to an existing
short private directory before running. Interrupts fail the run, clean up owned
processes, and retain diagnostic artifacts; SIGKILL of the harness itself cannot
guarantee cleanup. One-machine direct-path results are **not** WAN, relay, or
cross-OS acceptance.

Finalization has a separate 30-second cleanup budget, a three-second forced-reap
fallback and a five-second best-effort version probe. Log pumps drain for at most
two seconds. Version-probe failure still writes a failed summary; report-writing
failure still removes private runtime state. OS/filesystem hangs in synchronous
operations cannot be bounded by an asyncio timer.

## Docker TCP/HTTP stress harness

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

The soak runner accepts `--http-timeout-secs` and applies that value to both the
host and HTTP attachment. Use a short value only with a deliberately delayed
fixture; the normal HTTPS fixture is expected to complete within the timeout.

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

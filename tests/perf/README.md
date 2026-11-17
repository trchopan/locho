# Performance testing

Locho provides three performance harnesses:

| Harness | Purpose |
| --- | --- |
| [Two-machine native](#two-machine-native-stress-and-soak) | WAN forwarding, recovery, idle behavior, and resource checks over SSH |
| [Single-machine native](#native-unix-stream-smoke-stress-and-soak-tests) | Unix socket lifecycle and local stream forwarding |
| [Docker](#docker-tcphttp-stress-harness) | Isolated HTTP/TCP load against the current checkout |

Run commands from the repository root. Start with short tests, review artifacts,
and run suites sequentially. Results apply to the tested workload and environment;
they do not replace the [release checklist](../../docs/releasing.md).

## Two-machine native stress and soak

`two_machine.py` runs traffic and attachments on a controller and fixtures and a
Locho host on a target. Both need Python 3.11+, `ps`, and, on macOS, `lsof`.
Fixtures cover TCP→TCP, TCP→Unix, Unix→TCP, and Unix→Unix forwarding with checked
binary frames. No production backend or Docker installation is required.

`remote_pair.py` runs both measured machines remotely. The administrator carries
control RPC and collects artifacts; it does not generate workload traffic. Both
SSH routes must use batch authentication and verified host keys, independently of
the Locho transport under test. Keys are not copied or agent-forwarded.

### Workloads and durations

| `--workload` | Endurance (`stress` mode) | Soak | Bulk traffic |
| --- | --- | --- | --- |
| `supported-admin` | Four total workers | Four total workers | One ordinary 1 MiB request/response at a time, first due at 30s and then every 60s, rotating across services |
| `capacity` (default) | Ramps total concurrency: 4→8 in dev; 4→8→16→32→64 in acceptance | 12 total workers | Periodic 1 MiB transfers, including overlapping duplex traffic |

| `--profile` | Endurance | Soak |
| --- | --- | --- |
| `dev` | 2m default; 1–2m allowed | 3m default; 2–5m allowed |
| `acceptance` | Exactly 20m | Exactly 1h |

Concurrency is distributed across all four forwarding combinations, not multiplied
per service. Quiet connections and recovery probes reserve additional streams;
ordinary workloads stay below 128 active streams.

`supported-admin` is a synthetic interactive/admin workload, not a product-wide
connection limit. It uses four workers with a 0.1s pause after each operation and
small frames of 1 byte, 256 bytes, and 16 KiB. Small traffic includes connection
reuse, fragmentation, delayed reads, duplex exchanges, and half-close handling.
Bulk exchanges use an existing worker and do not deliberately fragment writes,
delay reads, or use duplex mode. Actual bulk start/finish times are recorded;
missed, interrupted, or failed bulk slots fail coverage and are never replayed.
Default dev runs require 2/3 bulk exchanges; full endurance/soak require 20/60.

Each operation has a **10-second completion deadline**, including connection setup
when needed. Durations are workload-phase budgets after readiness, including
recovery and drains; achieved traffic time is reported separately. Setup, cleanup,
and recovery are bounded to 120s, 30s, and 30s respectively, with an owned-process
forced-reap fallback.

### Running a remote pair

The launcher currently pins official Locho `1.3.0` and verifies release-archive
SHA-256 digests. The controller must be Linux x86-64; supported target platforms
are Linux x86-64 and Apple Silicon macOS. Binaries are staged privately, never
installed over existing executables.

Set the SSH destination variables for your environment and preview the workload:

```sh
python3 -B tests/perf/remote_pair.py both --profile dev \
  --workload supported-admin \
  --controller-ssh "$CONTROLLER_SSH" \
  --target-ssh-host "$TARGET_HOST" --target-ssh-port 22 \
  --target-ssh-user "$TARGET_USER" --target-python /usr/bin/python3 \
  --target-platform x86_64-unknown-linux-gnu \
  --stage-target-binary --dry-run
```

The dry run does not connect, download binaries, or start load. After reviewing it,
replace `--dry-run` with `--approve-execution`. That flag authorizes private staging,
fixtures, readiness traffic, workload, and scheduled run-owned faults. `both` runs
endurance then soak and stops if a phase fails. Run `smoke` first; custom-relay
execution prepends smoke automatically.

For full runs, use `--profile acceptance` and add `--approve-acceptance` as well as
`--approve-execution`. Use `stress` or `soak` instead of `both` to review each long
phase separately. The `stress` name is retained for CLI compatibility; with
`supported-admin` it is fixed-load endurance, not a capacity ramp.

For macOS targets, select `--target-platform aarch64-apple-darwin` and a suitable
Python 3.11+ path. To copy an existing target executable instead of uploading one,
replace `--stage-target-binary` with `--target-binary` and `--target-sha256`; it must
match the verified release. If the public GitHub API is rate-limited,
`--github-api-via-gh` uses existing GitHub CLI authentication for metadata without
changing archive verification.

Use `two_machine.py --help` for a local controller, explicit binary provenance,
and resource overrides. Use `remote_pair.py --help` for SSH and staging options.

### Custom relays

Add these options to the reviewed command:

```sh
--relay-url https://relay.example.org:8443 \
--controller-relay-env "$CONTROLLER_RELAY_ENV" \
--target-relay-env "$TARGET_RELAY_ENV"
```

The environment files must already exist on their respective measured machines,
be owned by the SSH user, and have no group/other access, including their parent
directories. Symlink sources are refused. Keep tokens out of CLI arguments and
retained artifacts. The default token variable is `LOCHO_RELAY_TOKEN`; use
`--expected-relay-version` to select the required relay health-check version.

Relay configuration is run-scoped. N0 relay fallback is excluded unless
`--include-n0-relays` is set; QAD remains enabled unless explicitly disabled.
Direct paths are still preferred. Custom-relay coverage requires an observed
custom path, but does not prove that every payload byte traversed the relay.

Optional `--relay-monitor-ssh` sampling reads numeric limits, process metrics, and
localhost relay counters through a separately verified SSH route with
non-interactive `sudo`. It does not read the relay credential environment or
change services. Counters cover the whole relay, including other clients. Missing
required samples fail monitoring; historical totals alone do not attribute a test
failure to throttling or packet drops.

### Pass criteria and safety

A run fails on corruption, replay, unexpected workload I/O or process failure,
missing required observations, failed recovery/idle gates, artifact-transfer
failure, or unverified cleanup. Scheduled-fault interruptions are reported
separately; only fresh recovery probes retry. Interrupted application operations
are never replayed.

The full soak holds four application-byte-quiet connections for **365 seconds**
with idle policies disabled and verifies default expiry within **290–320 seconds**.
It restarts fixtures, host, and attachment at 45%, 65%, and 80% of the workload
budget. Dev soak substitutes short idle checks and restarts only the attachment;
it cannot certify real idle behavior.

FD drain checks compare the same PID, generation, and start identity before
restarts, with a default allowance of 16 descriptors. `supported-admin` also
requires bounded stabilization of the warmed setup baseline. RSS, CPU, throughput,
and retained latency percentiles are diagnostics, not universal performance SLAs.

Use an independent SSH control route. Target watchdogs and process guards bound
owned-child lifetime on control loss; cleanup is not inferred from an SSH exit.
Failed-run diagnostics may remain staged for recovery. Never clean up by process
name or delete arbitrary temporary directories.

For unattended runs, `bounded_run.py` wraps a reviewed command after `--` with a
fresh `--output` directory and required `--timeout`. Typical coordinator budgets
are 2,000s for endurance and 4,400s for soak, including smoke and collection. On
macOS it holds a bounded idle-sleep assertion on battery or AC, without global
power changes. Keep the lid open, sufficient power available, and the network
reachable; forced sleep, shutdown, or connection loss can still interrupt control.

### Artifacts and diagnostics

Each phase writes a private `artifacts/two-machine-<mode>-<timestamp>-<UUID>/`
directory containing summary/metadata JSON, operation and timeline events,
controller/target metrics, bounded logs, and checksum-verified target diagnostics.
The remote launcher adds administration and staging/cleanup receipts.

Logs cap at 1 MiB per process, detailed operation events at 2,000, and retained
latencies at 10,000 samples. Aggregate failure counts continue after detail caps.
Controller collection allows at most 64 files, 8 MiB each, and 64 MiB total. Missing
gates and truncation are reported explicitly. Review artifacts before publishing:
redaction is not full anonymization, and endpoint addresses may remain present.

`--transfer-telemetry` and `--fixture-observation` enable bounded client/fixture I/O
progress without retaining payloads. Local write/drain completion is not remote
receipt. The `diagnostic` mode provides short fragmentation, duplex, mixed-flow,
staggered-start, and initial-versus-conditioned comparisons; see `--diagnostic-kind`
and `--no-conditioning-control`. These are investigations, not soak acceptance.
Transport comparisons require relay monitoring. Explicit instrumented candidates
require a checksum/provenance manifest and retained source/diff, are labeled
modified, and cannot satisfy the official-release `supported-admin` profile.

### Validation and limitations

Locho `1.3.0` passed the `supported-admin` profile on a Linux x86-64 controller and
Linux x86-64 container target on 2026-11-02:

| Phase | Checked operations | Scheduled bulk exchanges | Unexpected workload I/O, validation, or replay failures |
| --- | --- | --- | --- |
| 20-minute endurance | 20,991 | 20/20 | 0 |
| One-hour soak | 63,961 | 60/60 | 0 |

The full soak passed real idle, scheduled recovery, same-lifetime FD, artifact, and
cleanup gates. This is native forwarding evidence for the stated workload, not
certification of real SSH/HTTP semantics, SDKs, macOS, or forced relay-only behavior.
The summary's `acceptance_certified` field is scoped to its workload/profile, not
an overall product-release decision.

Higher-overlap bulk workloads have intermittently exceeded the 10-second deadline
on WAN paths. The cause remains unresolved; passing serial-bulk tests does not
establish a fix or a concurrent-bulk performance guarantee. Treat capacity results
as characterization unless that load is an explicit release requirement, and do
not silently change deadlines or relabel failed runs.

Local harness regression checks require neither SSH nor real Locho transport:

```sh
python3 -m unittest discover -s tests/perf -p 'test_*.py' -v
```

## Native Unix stream smoke, stress and soak tests

`unix_streams.py` tests filesystem Unix stream support using the **ordinary
release binary**, native processes and generic byte-stream fixtures. It needs
Python 3.11+, `ps`, and (on macOS) `lsof`. Fixtures are self-contained; Docker is
not required. Run it natively on both macOS and Linux: Docker Desktop socket tests
alone exercise its Linux VM, not macOS listener behavior.

```sh
cargo build --release
python3 -m unittest discover -s tests/perf -p test_unix_streams.py -v
python3 -m unittest discover -s tests/perf/loadgen -p test_loadgen.py -v
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
4, 8, 16 and 32 by default over a five-minute load budget. On macOS, the runner
uses 4, 8, 16 and 28 for the loaded stress phases because the platform's loopback
TCP TIME_WAIT/source-port behavior makes exactly 128 churn workers an OS resource
limit rather than a useful transport test. The dedicated budget probe still holds
and validates all 128 active streams. Override the matrix
or use one level when isolating a result:

```sh
python3 tests/perf/unix_streams.py stress --duration 5m --levels 4,8,16,32
python3 tests/perf/unix_streams.py stress --duration 2m --concurrency 16
```

Each level exercises TCP→TCP, TCP→Unix, Unix→TCP and Unix→Unix with persistent
connections, churn, fragmented/slow transfers and periodic 1 MiB payloads. The
two TCP upstream fixtures use separate loopback destination ports so macOS source
port/TIME_WAIT pools do not alias unrelated forwarding combinations. This is a
fixture-capacity isolation measure, not a production transport change.
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
  stress samples are taken during active phases as well as at phase boundaries;
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
It never uses existing Compose projects or production environment files. The
Locho image is built from the repository source with the `integration-test`
feature so the fixed direct-address path can be exercised.

### Requirements

- Docker Desktop with Compose v2
- OpenSSL and Python 3 on the host
- Network access for iroh discovery/relay bootstrap

Apple Silicon hosts should use the native ARM64 Docker platform. The fixtures
and load generator are architecture-independent.

### Running

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

### Artifacts

Each run creates `artifacts/stress-<UTC timestamp>-<pid>/` containing metadata,
per-phase JSON summaries and JSONL events, phase CSV, container stats, and logs.
The exit trap collects these files and removes the Compose containers, network,
and temporary runtime state even after an interrupted run. The artifact
directory is retained for analysis.

The load generator records successful operations, failures with reasons,
timeouts, connection resets, validation failures, throughput, and p50/p95/p99
latency. HTTP responses are checked against the deterministic fixture body, not
just their status and size; POST fixtures also consume and validate the request
body. HTTP request and response sizes, and TCP message sizes are configurable.
Events are streamed to JSONL and latency samples are bounded so long runs do not
retain every operation in memory. Container statistics are sampled every five
seconds by the collector, including restart count and exit status. No throughput
threshold is imposed; compare runs using the same machine and commit.

The soak runner accepts `--http-timeout-secs` and applies that value to both the
host and HTTP attachment. Use a short value only with a deliberately delayed
fixture; the normal HTTPS fixture is expected to complete within the timeout.

The standard stress baseline stays below Locho's documented TCP connection
limit. Run `--tcp-concurrency "1 10 50 100"` separately when validating the
limit and expected rejection behavior.

### Soak runs

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
artifacts are written to `artifacts/soak-<UTC timestamp>-<pid>/`. Recovery results
are in `timeline.jsonl`; transient connection failures during a restart are
allowed only for the recovery load and remain in the load-generator events.
Response corruption, unexpected status codes, and other validation failures
still fail the run. Successful operations are sampled and interval summaries
are emitted every 10 seconds by default, keeping long-run artifacts bounded.
Failure events are capped per load-generator process while interval counts
continue to include every failure. Override this with `--success-sample-rate`
and `--interval` when needed. Test native suspend/resume separately; Compose
does not simulate lid transitions on the host.

### Fixtures

`upstream_http` is a deterministic HTTPS server using a fresh run-specific CA
and certificate. It returns request metadata and a configurable response body.
`upstream_tcp` is a deterministic TCP echo server. Both are isolated on the
dedicated `locho-stress-network` network.
